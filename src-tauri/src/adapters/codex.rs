//! The Codex CLI adapter — rollout files in, neutral sessions out.
//!
//! Every number quoted below was measured on this Mac (the owner's, 2026-09-13) against Codex
//! 0.149.1: 72 rollout files under `~/.codex/sessions`, 68 logical sessions, 4 subagent rollouts,
//! 3 208 `token_count` events, largest single line 1.52 MB.
//!
//! Four things about this source are counter-intuitive enough to be worth stating once:
//!
//! 1. **A resume writes a NEW FILE for the same thread**, so a session is a *group* of files, not
//!    a file. Head fields come from the earliest file in the group; `last_active_ms` is the max
//!    mtime over it.
//! 2. **The sid is the whole UUID.** These are UUIDv7: the first 8 hex characters advance only
//!    every ~65 s, and Studio's ADR-0046 found 8 colliding pairs in 110 rollouts. Nothing here
//!    ever takes a prefix.
//! 3. **The naive "first user item" is never the title.** In all 68 title-bearing rollouts here
//!    the first user `response_item` is a wrapper (`<environment_context>` 32×, `# AGENTS.md` 32×,
//!    `<recommended_plugins>` 8×). The `user_message` *event* is the first thing the owner
//!    actually typed and exists in 50 of 72 files; the filtered `response_item` carries the other
//!    18.
//! 4. **`payload.rate_limits` is a sibling of `payload.info`, not a member of it** — the single
//!    most misread thing in this format, and the reason the nested path is tried *explicitly*
//!    below rather than left to a reader that would otherwise silently report a machine with no
//!    Codex on it.
//!
//! Read-only throughout: nothing here opens a file for writing, renames one, or takes a lock.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};

use super::wait::{OwnerWait, WaitPolicy, WaitSignal};
use super::{ProviderAdapter, ProviderSessionReport, SessionCandidate};
use crate::api::errors::{EngineError, ErrorDetail, ErrorKind};
use crate::domain::{
    Capacity, CapacityWindow, CapacityWindowName, Diagnostics, FileSignature, Identity, LiveState,
    Metrics, ProviderId, ResumeBlockedReason, Session, SessionKey, SourceSignature, SourceSummary,
};
use crate::util;

const PROVIDER: ProviderId = ProviderId::Codex;

const ROLLOUT_PREFIX: &str = "rollout-";
const ROLLOUT_SUFFIX: &str = ".jsonl";

/// The identity record is the first line of every one of the 72 rollouts here; three lines of
/// slack covers a blank or half-written leader without reading a whole 18 MB file to find out.
const HEAD_LINES: usize = 3;

/// How far into a rollout the title hunt goes before giving up on the preferred candidate. The
/// `user_message` event sits in the first few KB of every file that has one; 1 MB is Studio's
/// extended budget and keeps a 68-group discovery off the multi-megabyte tails.
const TITLE_SCAN_BUDGET: u64 = 1 << 20;

/// Newest rollouts consulted for a capacity figure. A newest file can be a subagent's or a
/// session that never reached a model, so one file is not enough to conclude "no figure".
const CAPACITY_FILES: usize = 10;

/// Tail windows, grown on a miss. Measured 2026-09-13: the last `token_count` carrying
/// `rate_limits` sat between 1.2 KB and 408 KB from EOF across the 10 newest rollouts, in files
/// of up to 18 MB — so 64 KB answers most, and 4 MB is the point at which giving up beats
/// reading a whole file for a capacity dial.
const TAIL_BUDGETS: [u64; 3] = [64 << 10, 512 << 10, 4 << 20];

/// Older than its own shortest window (5 h) would be generous; an hour is the point past which
/// the owner should be told the figure is a memory, not a reading.
const STALE_AFTER_S: u64 = 3600;

/// Scaffolding the CLI injects as a user turn. The first user `response_item` is one of these in
/// every sampled file, which is why the naive title is wrong 68 times out of 68.
const WRAPPERS: [&str; 4] = [
    "<environment_context>",
    "<user_instructions>",
    "<recommended_plugins>",
    "# AGENTS.md",
];

/// Envelope types seen in this corpus. Anything else is noted in [`Diagnostics`] rather than
/// ignored: formats drift, and a silent skip is how a wrong number gets rendered.
const KNOWN_TYPES: [&str; 6] = [
    "session_meta",
    "event_msg",
    "response_item",
    "turn_context",
    "compacted",
    "world_state",
];

/// `response_item` payload types that are a tool invocation. Measured here: 1 506
/// `custom_tool_call`, 1 233 `function_call`, 0 `local_shell_call` (kept because Studio's corpus
/// has it).
const TOOL_CALL_TYPES: [&str; 3] = ["function_call", "custom_tool_call", "local_shell_call"];

/// The Codex adapter. `home` is the engine's own root, so a test can point it at a fixture tree
/// without an environment variable that concurrent tests would fight over.
pub struct CodexAdapter {
    home: PathBuf,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            home: default_home(),
        }
    }

    /// Point the adapter at an explicit `CODEX_HOME`-shaped directory.
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    fn sessions_root(&self) -> PathBuf {
        self.home.join("sessions")
    }

    /// Both roots, in the order they are searched. `archived_sessions` is absent on this Mac and
    /// is skipped rather than reported — an engine that has archived nothing is not an error.
    fn rollout_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.sessions_root()];
        let archived = self.home.join("archived_sessions");
        if archived.is_dir() {
            roots.push(archived);
        }
        roots
    }

    fn all_rollouts(&self) -> Vec<FileFact> {
        let mut out = Vec::new();
        for root in self.rollout_roots() {
            collect_rollouts(&root, &mut out);
        }
        out
    }

    fn discover(&self) -> Result<Vec<SessionCandidate>, EngineError> {
        let sessions = self.sessions_root();
        if !sessions.is_dir() {
            return Err(EngineError::root_missing(PROVIDER, &sessions));
        }
        let files = self.all_rollouts();
        let names = read_session_index(&self.home.join("session_index.jsonl"));

        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        let mut heads_read = 0usize;
        for fact in files.iter() {
            let Some(head) = read_head(&fact.path) else {
                continue;
            };
            heads_read += 1;
            // A subagent's rollout is a tree under someone else's session, not a resumable
            // session: it carries the PARENT's id in `session_id` while `id` names its own
            // thread. Studio measured that preferring `session_id` absorbed 83 of 103 rollouts
            // into their parents (backlog #40); 4 of the 72 files here are subagents.
            if head.is_subagent {
                continue;
            }
            let entry = groups.entry(head.own_id.clone()).or_insert_with(|| Group {
                head: head.clone(),
                head_path: fact.path.clone(),
                head_order: order_key(&head, fact),
                files: Vec::new(),
            });
            let order = order_key(&head, fact);
            if order < entry.head_order {
                entry.head = head;
                entry.head_path = fact.path.clone();
                entry.head_order = order;
            }
            entry.files.push(fact.clone());
        }

        // Rollouts present but not one readable identity is drift in the head record itself, and
        // an empty list would read as "no Codex work on this machine" — the exact silent wrong
        // answer the fail-loud rule exists for.
        if heads_read == 0 && !files.is_empty() {
            return Err(EngineError::unknown_shape(
                PROVIDER,
                &["session_meta.payload.id"],
            ));
        }

        Ok(groups
            .into_values()
            .map(|group| group.into_candidate(&names))
            .collect())
    }

    /// Capacity as of `now_ms`. Split from [`ProviderAdapter::read_capacity`] so staleness can be
    /// tested against a real file without setting an mtime the standard library cannot set.
    fn capacity_at(&self, now_ms: i64) -> Capacity {
        let sessions = self.sessions_root();
        if !sessions.is_dir() {
            let problem = EngineError::root_missing(PROVIDER, &sessions);
            return Capacity::problem(PROVIDER, now_ms, problem);
        }
        let mut files = self.all_rollouts();
        files.sort_by(|a, b| {
            b.mtime_ms
                .cmp(&a.mtime_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        files.truncate(CAPACITY_FILES);

        // The newest rollout whose tail budget ran out before byte 0, if any. It is what turns the
        // closing error from a statement about Codex's format into a statement about our own
        // reading — see [`TailSearch`].
        let mut unread: Option<PathBuf> = None;
        for fact in files {
            // Both needles on one line before any JSON work: a rollout is up to 18 MB and the
            // substring filter is what keeps this read cheap.
            let needles = ["\"token_count\"", "\"rate_limits\""];
            let line = match last_line_containing(&fact.path, &needles) {
                TailSearch::Found(line) => line,
                TailSearch::Absent => continue,
                TailSearch::Unread => {
                    unread.get_or_insert(fact.path.clone());
                    continue;
                }
            };
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(payload) = record.get("payload") else {
                continue;
            };
            let limits = payload
                .get("rate_limits")
                .or_else(|| payload.get("info").and_then(|info| info.get("rate_limits")));
            let Some(limits) = limits.and_then(Value::as_object) else {
                continue;
            };
            return capacity_from_limits(limits, fact.mtime_ms, now_ms);
        }
        // Two different failures, and conflating them is how a 17 MB rollout gets reported as
        // drift in a schema that never moved. `UnknownShape` is only honest where every consulted
        // file was read end to end and none stated the field; where a budget stopped us first, the
        // truthful statement is that the read did not happen — `ErrorKind::Io` with the path,
        // which is the same thing `services/accounts.rs` says of a read it could not perform.
        let problem = match unread {
            Some(path) => EngineError::with(
                PROVIDER,
                ErrorKind::Io,
                ErrorDetail::Path {
                    path: path.display().to_string(),
                },
            ),
            None => EngineError::unknown_shape(PROVIDER, &["payload.rate_limits"]),
        };
        Capacity::problem(PROVIDER, now_ms, problem)
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderAdapter for CodexAdapter {
    fn provider(&self) -> ProviderId {
        PROVIDER
    }

    fn discover_sessions(&self) -> ProviderSessionReport {
        match self.discover() {
            Ok(sessions) => ProviderSessionReport {
                sessions,
                problem: None,
            },
            Err(problem) => ProviderSessionReport::problem(problem),
        }
    }

    fn read_identity(&self) -> Identity {
        let now_ms = util::now_ms();
        let path = self.home.join("auth.json");
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(err) => {
                let problem = if err.kind() == std::io::ErrorKind::NotFound {
                    EngineError::of(PROVIDER, ErrorKind::NoCredential)
                } else {
                    EngineError::from_io(PROVIDER, &err, &path)
                };
                return Identity::absent(PROVIDER, now_ms, Some(problem));
            }
        };
        // `AuthFile` has no field for `access_token`, `id_token`, `refresh_token` or
        // `OPENAI_API_KEY`, so serde walks past them and no token value is ever held. The error
        // below is built from the fields we WANTED and never from serde's own message, which
        // quotes the offending input — in this file, a credential.
        let auth: AuthFile = match serde_json::from_reader(BufReader::new(file)) {
            Ok(auth) => auth,
            Err(_) => {
                let problem =
                    EngineError::unknown_shape(PROVIDER, &["auth_mode", "tokens.account_id"]);
                return Identity::absent(PROVIDER, now_ms, Some(problem));
            }
        };
        let mode = auth.auth_mode.filter(|mode| !mode.trim().is_empty());
        let account_short = auth
            .tokens
            .and_then(|tokens| tokens.account_id)
            .map(|id| last_chars(&id, 6))
            .filter(|tail| !tail.is_empty());
        // The plan word is the engine's own and is only stated in the rate-limit record — there
        // is no plan field in `auth.json`. An absent capacity reading simply means no plan.
        let plan = if mode.is_some() {
            self.capacity_at(now_ms).plan
        } else {
            None
        };
        Identity {
            provider: PROVIDER,
            signed_in: mode.is_some(),
            label: None,
            organization: None,
            plan,
            tier: None,
            mode,
            account_short,
            providers: None,
            read_at_ms: now_ms,
            problem: None,
        }
    }

    fn read_capacity(&self) -> Capacity {
        self.capacity_at(util::now_ms())
    }
}

/// Counters for one session, from every file in its group.
///
/// **Token totals are taken, not summed.** `payload.info.total_token_usage` is cumulative over
/// the thread, so the LAST `token_count` event already *is* the session's total; adding them
/// would multiply the answer by the number of turns. The per-turn `last_token_usage` deltas are
/// deliberately not used — Studio cross-checks that they sum to this same figure, so the tail
/// read is the cheaper of two equal answers. (Open, and unmeasurable here: whether a *resume*
/// restarts the cumulative counter. Zero of the 68 sessions on this Mac span two files, so there
/// is no evidence either way; if one ever does and the totals restart, this is the line that
/// needs a per-file last-value sum.)
///
/// **`input_tokens` has the cached portion SUBTRACTED, and that is not a correction to the
/// engine — it is a units conversion.** Codex's `input_tokens` is a superset that already
/// contains `cached_input_tokens`; measured on this Mac 2026-09-13, one session reported
/// 12,208,937 input tokens of which 11,544,064 were cached, and `total_tokens` = `input_tokens` +
/// `output_tokens` exactly. Claude's API reports the two as *disjoint* quantities, so
/// `Metrics::input_tokens` means "uncached input" everywhere else in this product. Passing
/// Codex's figure straight through would double-count the cached prompt (once here, once in
/// `cache_read`) AND make a Codex row incomparable with a Claude row in the project card that
/// sums them. So `input_tokens = input_tokens − cached_input_tokens`, saturating at 0, per
/// `docs/contracts/frds.md` §2.2.5. Do not "fix" this back.
///
/// **A user turn is a `user_message` event, or — only when a file states none — that file's
/// wrapper-filtered user `response_item`s.** 22 of the 72 rollouts here emit no `user_message`
/// event at all (the paginated-history shape, which includes all three of the newest sessions),
/// and rendering those as 0 turns would be a wrong number rather than an absence. The rule is per
/// FILE, so a resume that states events never suppresses an earlier file's fallback.
///
/// **A group with no `token_count` at all is not a failure.** 12 of the 68 sessions here are real
/// zero-call sessions (the owner opened Codex and closed it). They return the tool and turn
/// counts that were positively parsed, with the token counters and `api_calls` at 0 — and
/// `api_calls == 0` makes every KPI `None` through [`crate::domain::ratio`], so the View states
/// an absence instead of drawing a zero bar. An unreadable file, by contrast, is an `Err`.
pub fn read_metrics(files: &[PathBuf]) -> Result<Metrics, EngineError> {
    if files.is_empty() {
        return Err(EngineError::of(PROVIDER, ErrorKind::Path));
    }
    let mut ordered: Vec<(i64, PathBuf)> = Vec::with_capacity(files.len());
    for path in files {
        let meta =
            std::fs::metadata(path).map_err(|err| EngineError::from_io(PROVIDER, &err, path))?;
        ordered.push((util::mtime_ms(&meta), path.clone()));
    }
    // Chronological, so "the last token_count" is well defined whatever order the caller passed.
    ordered.sort();

    let mut metrics = Metrics::default();
    let mut totals: Option<Totals> = None;
    let mut first_ms: Option<i64> = None;
    let mut last_ms: Option<i64> = None;

    for (_, path) in &ordered {
        let file = File::open(path).map_err(|err| EngineError::from_io(PROVIDER, &err, path))?;
        let mut reader = BufReader::new(file);
        let mut buf: Vec<u8> = Vec::new();
        // Per FILE, because the fallback below is decided per file: a resumed thread can state
        // its turns as events in one rollout and as response items in the next.
        let mut user_events: u64 = 0;
        let mut user_items: u64 = 0;
        loop {
            buf.clear();
            // `read_until` over bytes, never a capped buffer and never `lines()`: a rollout line
            // reaches 1.52 MB here, and a live file's final line can be a half-written UTF-8
            // sequence that `read_line` would turn into an error for the whole session.
            let read = reader
                .read_until(b'\n', &mut buf)
                .map_err(|err| EngineError::from_io(PROVIDER, &err, path))?;
            if read == 0 {
                break;
            }
            // A line that does not parse is a line being appended to right now — Codex writes
            // these files live. Studio's record reader takes the same per-line skip for the same
            // I/O reality. Drift is asserted on a record we DID parse and did not understand.
            let Ok(record) = serde_json::from_slice::<Value>(&buf) else {
                continue;
            };
            if let Some(ms) = record
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_iso_ms)
            {
                first_ms = Some(first_ms.map_or(ms, |cur: i64| cur.min(ms)));
                last_ms = Some(last_ms.map_or(ms, |cur: i64| cur.max(ms)));
            }
            let kind = record
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let payload = record.get("payload");
            let payload_type = payload
                .and_then(|p| p.get("type"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            match kind {
                "event_msg" => match payload_type {
                    "token_count" => {
                        metrics.api_calls += 1;
                        if let Some(parsed) = read_total_usage(payload)? {
                            totals = Some(parsed);
                        }
                    }
                    "user_message" => user_events += 1,
                    _ => {}
                },
                "response_item" => {
                    if TOOL_CALL_TYPES.contains(&payload_type) {
                        metrics.tool_calls += 1;
                    } else if payload_type == "message" && plain_user_message(payload).is_some() {
                        user_items += 1;
                    }
                }
                _ => {}
            }
        }
        // Rule 1 where the file states events, rule 2 where it states none. Never both: the
        // response items are the same words restated, so adding them would double every turn in
        // the 50 of 72 files that carry both.
        metrics.user_turns += if user_events > 0 {
            user_events
        } else {
            user_items
        };
    }

    if let Some(totals) = totals {
        // The units conversion the doc comment above argues for. `saturating_sub` can only bite
        // if the guard in `read_total_usage` is ever relaxed; it is belt and braces, not a clamp.
        metrics.input_tokens = totals.input.saturating_sub(totals.cached);
        metrics.output_tokens = totals.output;
        metrics.cache_read = totals.cached;
        metrics.cache_write = totals.cache_write;
        metrics.reasoning_tokens = Some(totals.reasoning);
    }
    metrics.duration_ms = match (first_ms, last_ms) {
        (Some(first), Some(last)) if last >= first => Some((last - first) as u64),
        _ => None,
    };
    // Codex is a subscription login here. Pigeon never invents a price, so the cost is absent
    // rather than 0.0, which would read as "this session was free".
    metrics.provider_cost_usd = None;
    Ok(metrics)
}

// ------------------------------------------------------------------------------------------- //
// Rollout files
// ------------------------------------------------------------------------------------------- //

#[derive(Clone, Debug)]
struct FileFact {
    path: PathBuf,
    mtime_ms: i64,
    size: u64,
}

#[derive(Clone, Debug)]
struct Head {
    own_id: String,
    parent_id: Option<String>,
    is_subagent: bool,
    cwd: Option<PathBuf>,
    git_branch: Option<String>,
    started_ms: Option<i64>,
}

struct Group {
    head: Head,
    head_path: PathBuf,
    head_order: (i64, PathBuf),
    files: Vec<FileFact>,
}

impl Group {
    fn into_candidate(mut self, names: &BTreeMap<String, String>) -> SessionCandidate {
        // Newest first, because `SourceSummary::display` shows the first path and calls it the
        // newest. The signature keeps its own path-sorted order so it is stable across reads.
        self.files.sort_by(|a, b| {
            b.mtime_ms
                .cmp(&a.mtime_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        let last_active_ms = self.files.iter().map(|f| f.mtime_ms).max().unwrap_or(0);
        let mut diagnostics = Diagnostics::default();
        let title = scan_title(&self.head_path, &mut diagnostics)
            .map(|raw| util::tidy_title(&raw, 120))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| Session::UNTITLED.to_string());

        let (resumable, resume_blocked_reason) = match self.head.cwd.as_deref() {
            None => (false, Some(ResumeBlockedReason::MissingWorkingDirectory)),
            Some(cwd) if !cwd.is_dir() => {
                (false, Some(ResumeBlockedReason::WorkingDirectoryMissing))
            }
            Some(_) => (true, None),
        };

        let mut signatures: Vec<FileSignature> = self
            .files
            .iter()
            .map(|f| FileSignature {
                path: f.path.clone(),
                size: f.size,
                mtime_ms: f.mtime_ms,
            })
            .collect();
        signatures.sort_by(|a, b| a.path.cmp(&b.path));
        let paths = self.files.iter().map(|f| f.path.clone()).collect();

        SessionCandidate {
            key: SessionKey::new(PROVIDER, self.head.own_id.clone()),
            cwd: self.head.cwd.clone(),
            title,
            // The owner's own name for the thread. It is a NAME, not the title: the title still
            // says what was asked, and only 12 of 71 threads here are named at all.
            name: names.get(&self.head.own_id).cloned(),
            git_branch: self.head.git_branch.clone(),
            first_active_ms: self.head.started_ms,
            last_active_ms,
            closed_at_ms: None,
            resumable,
            resume_blocked_reason,
            source: SourceSummary::many(paths),
            diagnostics,
            source_signature: SourceSignature::Codex { files: signatures },
        }
    }
}

/// Earliest-first ordering for picking a group's head file: the `session_meta` timestamp, with
/// the path as the tie-break so two files written in the same millisecond still order stably.
fn order_key(head: &Head, fact: &FileFact) -> (i64, PathBuf) {
    (head.started_ms.unwrap_or(fact.mtime_ms), fact.path.clone())
}

fn default_home() -> PathBuf {
    if let Some(explicit) = std::env::var_os("CODEX_HOME") {
        let path = PathBuf::from(explicit);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    dirs::home_dir().unwrap_or_default().join(".codex")
}

/// Every `rollout-*.jsonl` under `root`, walked with `std::fs` — the tree is date-partitioned
/// (`YYYY/MM/DD`) and shallow, so a recursive `read_dir` needs no crate.
fn collect_rollouts(root: &Path, out: &mut Vec<FileFact>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_rollouts(&path, out);
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(ROLLOUT_PREFIX) || !name.ends_with(ROLLOUT_SUFFIX) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        out.push(FileFact {
            path,
            mtime_ms: util::mtime_ms(&meta),
            size: meta.len(),
        });
    }
}

/// The identity of one rollout from its `session_meta` record, or `None` when it has none.
///
/// Ported from Studio's `capture/discovery.py::_codex_rollout_identity`. Own id is `payload.id`,
/// falling back to `session_id` for the oldest metas that carry only one key; a `session_id` that
/// names a *different* thread is the parent edge, and marks this file as a subagent's.
fn read_head(path: &Path) -> Option<Head> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    for _ in 0..HEAD_LINES {
        line.clear();
        // The head record carries the whole system prompt and runs to tens of KB; the buffer is
        // uncapped for the same reason the metrics reader's is.
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("session_meta") {
            return None;
        }
        let payload = record.get("payload")?;
        let own_id = text(payload.get("id")).or_else(|| text(payload.get("session_id")))?;
        let session_id = text(payload.get("session_id"));
        let parent_id = text(payload.get("parent_thread_id")).or_else(|| {
            session_id
                .as_deref()
                .filter(|sid| *sid != own_id)
                .map(str::to_string)
        });
        let thread_source = text(payload.get("thread_source"));
        let is_subagent = thread_source.as_deref() == Some("subagent") || parent_id.is_some();
        let started_ms = text(payload.get("timestamp"))
            .as_deref()
            .and_then(parse_iso_ms)
            .or_else(|| {
                text(record.get("timestamp"))
                    .as_deref()
                    .and_then(parse_iso_ms)
            });
        return Some(Head {
            own_id,
            parent_id,
            is_subagent,
            cwd: text(payload.get("cwd")).map(PathBuf::from),
            git_branch: payload.get("git").and_then(|git| text(git.get("branch"))),
            started_ms,
        });
    }
    None
}

/// The owner's own thread names, keyed by whole id. Read once per discovery: it is one small file
/// (12 lines here) and re-reading it per session would be 68 opens for the same bytes.
fn read_session_index(path: &Path) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    let Ok(file) = File::open(path) else {
        return names;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let (Some(id), Some(name)) = (text(record.get("id")), text(record.get("thread_name")))
        else {
            continue;
        };
        // Last write wins: the file is appended to, so a renamed thread states its new name last.
        names.insert(id, name);
    }
    names
}

/// The session's title, by the chain in the module docs. Returns the raw text; the caller tidies.
fn scan_title(path: &Path, diagnostics: &mut Diagnostics) -> Option<String> {
    let Ok(file) = File::open(path) else {
        return None;
    };
    let mut reader = BufReader::new(file);
    let mut buf: Vec<u8> = Vec::new();
    let mut scanned: u64 = 0;
    let mut fallback: Option<String> = None;
    loop {
        buf.clear();
        let Ok(read) = reader.read_until(b'\n', &mut buf) else {
            break;
        };
        if read == 0 {
            break;
        }
        scanned += read as u64;
        let Ok(record) = serde_json::from_slice::<Value>(&buf) else {
            if scanned > TITLE_SCAN_BUDGET {
                break;
            }
            continue;
        };
        let kind = record
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !KNOWN_TYPES.contains(&kind) {
            diagnostics.note(kind);
        }
        let payload = record.get("payload");
        let payload_type = payload
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if kind == "event_msg" && payload_type == "user_message" {
            if let Some(message) = payload.and_then(|p| text(p.get("message"))) {
                // The preferred candidate. Nothing later can beat it, so the scan stops here —
                // and it is chosen by TIER, not by file position: measured on this Mac, the
                // response_item copy of the same words is usually written first.
                return Some(message);
            }
        }
        if kind == "response_item" && payload_type == "message" && fallback.is_none() {
            fallback = plain_user_message(payload);
        }
        if scanned > TITLE_SCAN_BUDGET {
            break;
        }
    }
    fallback
}

/// A user `response_item`'s text, when it is something the owner wrote rather than scaffolding
/// the CLI injected.
///
/// One filter, two callers — the title chain and the user-turn fallback — because a wrapper that
/// counts as a turn but not as a title (or the reverse) would be two different definitions of
/// "the owner said something" in one adapter.
fn plain_user_message(payload: Option<&Value>) -> Option<String> {
    let payload = payload?;
    if text(payload.get("role")).as_deref() != Some("user") {
        return None;
    }
    let joined = joined_text(payload.get("content"))?;
    let head = joined.trim_start();
    if head.is_empty() || WRAPPERS.iter().any(|wrapper| head.starts_with(wrapper)) {
        return None;
    }
    Some(joined)
}

/// `content[].text`, joined. A user item's content is a list of typed parts; only the text ones
/// carry what the owner wrote.
fn joined_text(content: Option<&Value>) -> Option<String> {
    let parts = content?.as_array()?;
    let mut out = String::new();
    for part in parts {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    if out.trim().is_empty() {
        None
    } else {
        Some(out)
    }
}

// ------------------------------------------------------------------------------------------- //
// Counters
// ------------------------------------------------------------------------------------------- //

#[derive(Clone, Copy, Debug, Default)]
struct Totals {
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
}

/// `payload.info.total_token_usage`, or `Ok(None)` when this event states none.
///
/// Measured 2026-09-13: 35 of 3 208 `token_count` events carry no `info` object at all, and
/// `cache_write_input_tokens` is absent from 1 504 of the 3 173 that do. Both are ordinary and
/// must not fail. An `info` that is present but not an object, or a usage field present but not a
/// non-negative integer, IS drift and fails loud — that is the difference between a field the
/// engine omitted and a schema we no longer understand.
fn read_total_usage(payload: Option<&Value>) -> Result<Option<Totals>, EngineError> {
    let Some(info) = payload.and_then(|p| p.get("info")) else {
        return Ok(None);
    };
    if info.is_null() {
        return Ok(None);
    }
    let Some(info) = info.as_object() else {
        return Err(EngineError::unknown_shape(PROVIDER, &["payload.info"]));
    };
    let Some(usage) = info.get("total_token_usage") else {
        return Ok(None);
    };
    let Some(usage) = usage.as_object() else {
        return Err(EngineError::unknown_shape(
            PROVIDER,
            &["payload.info.total_token_usage"],
        ));
    };
    let totals = Totals {
        input: usage_field(usage, "input_tokens")?,
        cached: usage_field(usage, "cached_input_tokens")?,
        cache_write: usage_field(usage, "cache_write_input_tokens")?,
        output: usage_field(usage, "output_tokens")?,
        reasoning: usage_field(usage, "reasoning_output_tokens")?,
    };
    // The wire's `input_tokens` contains the cached portion, so a cached figure larger than it
    // means the relationship this mapping rests on has changed — and it is the relationship the
    // `input − cached` subtraction in `read_metrics` depends on, which would otherwise underflow
    // into a plausible-looking number. Never clamped: 0 of 3 173 records here breach it, so one
    // that does is news.
    if totals.cached > totals.input {
        return Err(EngineError::unknown_shape(
            PROVIDER,
            &["payload.info.total_token_usage.cached_input_tokens <= input_tokens"],
        ));
    }
    Ok(Some(totals))
}

fn usage_field(usage: &Map<String, Value>, key: &str) -> Result<u64, EngineError> {
    match usage.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(value) => value.as_u64().ok_or_else(|| {
            EngineError::with(
                PROVIDER,
                ErrorKind::UnknownShape,
                ErrorDetail::Fields {
                    fields: vec![format!("payload.info.total_token_usage.{key}")],
                },
            )
        }),
    }
}

// ------------------------------------------------------------------------------------------- //
// Capacity
// ------------------------------------------------------------------------------------------- //

/// One rate-limit record turned into windows. `mtime_ms` belongs to the file that answered, not
/// to the newest file on disk — the age the owner is shown is the age of the figure.
fn capacity_from_limits(limits: &Map<String, Value>, mtime_ms: i64, now_ms: i64) -> Capacity {
    let age_s = ((now_ms - mtime_ms).max(0) / 1000) as u64;
    let stale = age_s > STALE_AFTER_S;
    let plan = text(limits.get("plan_type"));
    let reached_limit = text(limits.get("rate_limit_reached_type"));

    let mut windows: Vec<CapacityWindow> = Vec::new();
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    let mut drift: Vec<String> = Vec::new();
    for slot in ["primary", "secondary"] {
        // An empty slot is ordinary: Studio's 2026-08-08 sample carried the weekly window alone.
        match limits.get(slot) {
            None | Some(Value::Null) => continue,
            Some(Value::Object(window)) => {
                let Some(minutes) = window.get("window_minutes").and_then(Value::as_u64) else {
                    drift.push(format!("rate_limits.{slot}.window_minutes"));
                    continue;
                };
                let minutes = minutes.min(u32::MAX as u64) as u32;
                // **A window is identified by its own `window_minutes`, never by the slot it
                // arrived in.** A live sample carried the weekly window in `primary` with
                // `secondary: null`, so slot order says nothing. Minutes we do not recognise are
                // dropped rather than guessed into a slot: Codex adding a third cap says nothing
                // about whether the two we do know were read correctly.
                let Some(name) = CapacityWindowName::from_minutes(minutes) else {
                    continue;
                };
                let Some(used_pct) = window.get("used_percent").and_then(Value::as_f64) else {
                    drift.push(format!("rate_limits.{slot}.used_percent"));
                    continue;
                };
                // Two slots claiming one window has never been seen; the first wins rather than
                // the list carrying the same allowance twice.
                if !seen.insert(minutes) {
                    continue;
                }
                // `resets_at` is epoch SECONDS on this wire and milliseconds everywhere inside
                // Pigeon. The one conversion lives here.
                let resets_at_ms = window
                    .get("resets_at")
                    .and_then(Value::as_i64)
                    .map(|secs| secs * 1000);
                windows.push(CapacityWindow {
                    name,
                    window_minutes: minutes,
                    used_pct,
                    resets_at_ms,
                });
            }
            Some(_) => drift.push(format!("rate_limits.{slot}")),
        }
    }

    let base = Capacity {
        provider: PROVIDER,
        supported: false,
        windows: Vec::new(),
        plan,
        stale,
        source_age_s: Some(age_s),
        reached_limit: reached_limit.clone(),
        read_at_ms: now_ms,
        problem: None,
    };

    if !drift.is_empty() {
        let problem = EngineError::with(
            PROVIDER,
            ErrorKind::UnknownShape,
            ErrorDetail::Fields { fields: drift },
        );
        return Capacity {
            problem: Some(problem),
            ..base
        };
    }
    if windows.is_empty() {
        // Measured on this Mac 2026-09-10: when the workspace ran out of credits mid-session both
        // windows came back null with `rate_limit_reached_type` naming why. That is a stated
        // condition, not a parser gap — and it draws NO bars, because the previous rollout's live
        // windows over a blocked account would be a green dial on an account that cannot run.
        let problem = match &reached_limit {
            Some(word) => EngineError::with(
                PROVIDER,
                ErrorKind::Unsupported,
                ErrorDetail::Word { word: word.clone() },
            ),
            None => EngineError::unknown_shape(
                PROVIDER,
                &[
                    "rate_limits.primary.window_minutes",
                    "rate_limits.secondary.window_minutes",
                ],
            ),
        };
        return Capacity {
            problem: Some(problem),
            ..base
        };
    }
    // Stale is a FLAG, not an error: the figure is real and is still shown, with its age beside
    // it. Pigeon never runs `codex` to mint a fresher one — a refresh would spend one of the
    // owner's own paid turns to read a dial.
    Capacity {
        supported: true,
        windows,
        ..base
    }
}

/// What a backwards tail search concluded. **Three answers, not two** — and the third is the whole
/// reason this is an enum: "it is not there" and "we never looked at all of it" are different
/// facts, and only one of them is evidence about the engine's format.
enum TailSearch {
    /// The last COMPLETE line carrying every needle.
    Found(String),
    /// Every byte of the file was covered and no complete line carried them. The needles are
    /// genuinely absent from this rollout.
    Absent,
    /// The largest budget ran out before byte 0, or the file could not be read at all. Nothing is
    /// known about the bytes that were never looked at.
    Unread,
}

/// The last **complete** line in `path` containing every needle, searched backwards from EOF.
///
/// Windows grow 64 KB → 512 KB → 4 MB, and a window has a partial line at *both* ends. The leading
/// one is dropped whenever the window does not start at byte 0. **The trailing one is dropped
/// whenever the file does not end in a newline** — the ordinary state of a rollout Codex is
/// appending to right now. Its half-written final record can contain both needles, and `.last()`
/// would then hand back JSON that cannot parse, costing the owner's own live session its turn at
/// answering in favour of an older file's staler figure, or of no figure at all.
///
/// Reading the whole file is deliberately not attempted. Measured on this Mac 2026-09-13: eight
/// rollouts exceed 4 MB and the largest is 17.4 MB, so once `len > 4 MB` the first `len − 4 MB`
/// bytes are never read. That is [`TailSearch::Unread`] and it is **not** [`TailSearch::Absent`]:
/// giving up is the right behaviour, but reporting a budget we chose as a field the engine did not
/// write would be a claim about a schema nobody looked at.
fn last_line_containing(path: &Path, needles: &[&str]) -> TailSearch {
    let Ok(mut file) = File::open(path) else {
        return TailSearch::Unread;
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return TailSearch::Unread;
    };
    for budget in TAIL_BUDGETS {
        let start = len.saturating_sub(budget);
        if file.seek(SeekFrom::Start(start)).is_err() {
            return TailSearch::Unread;
        }
        let mut buf: Vec<u8> = Vec::with_capacity((len - start) as usize + 1);
        if file.read_to_end(&mut buf).is_err() {
            return TailSearch::Unread;
        }
        // Every window runs to EOF, so a window that does not end in a newline is a file that does
        // not: its final element is a record still being written.
        let complete_tail = buf.last() == Some(&b'\n');
        let text = String::from_utf8_lossy(&buf);
        let skip = usize::from(start > 0);
        // Collected rather than streamed, because both ends have to be trimmed by index and
        // `Split` is not double-ended once skipped. The collected window IS double-ended, so the
        // search runs backwards from the last complete line and stops at the first match.
        let mut lines: Vec<&str> = text.split('\n').skip(skip).collect();
        if !complete_tail {
            lines.pop();
        }
        let found = lines
            .into_iter()
            .rfind(|line| needles.iter().all(|needle| line.contains(needle)));
        if let Some(line) = found {
            return TailSearch::Found(line.to_string());
        }
        if start == 0 {
            return TailSearch::Absent;
        }
    }
    TailSearch::Unread
}

// ------------------------------------------------------------------------------------------- //
// Identity
// ------------------------------------------------------------------------------------------- //

/// `auth.json`, with **no field for any token**. `access_token`, `id_token`, `refresh_token` and
/// `OPENAI_API_KEY` are in that file and are walked past by serde: they never become a value this
/// process holds, so they cannot reach a struct, a log line or an error.
#[derive(Debug, Deserialize)]
struct AuthFile {
    auth_mode: Option<String>,
    tokens: Option<AuthTokens>,
}

#[derive(Debug, Deserialize)]
struct AuthTokens {
    account_id: Option<String>,
}

/// The last `n` characters. A bounded tail tells two logins apart without publishing an id.
fn last_chars(value: &str, n: usize) -> String {
    let chars: Vec<char> = value.trim().chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}

// ------------------------------------------------------------------------------------------- //
// Small shared helpers
// ------------------------------------------------------------------------------------------- //

/// A non-empty trimmed string field, or `None`.
fn text(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?.trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

// ------------------------------------------------------------------------------------------- //
// Owner interactions: permission, question, interruption
// ------------------------------------------------------------------------------------------- //

/// How much of a rollout is read at a time while finding the last turn marker.
///
/// Codex can append megabytes of tool output after `task_started`. A fixed tail window therefore
/// turns a healthy, actively working session into `Unknown`. The reader walks backwards in chunks
/// so memory stays bounded while the search remains correct for arbitrarily long turns.
const ROLLOUT_SCAN_CHUNK_BYTES: u64 = 1024 * 1024;

/// Whether the rollout's last turn is open or closed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexTurn {
    Open,
    /// Carries the engine's own word: `task_complete` or `turn_aborted`.
    Closed(String),
}

/// Walk a rollout backwards and find the last turn marker.
///
/// **Why this is reliable enough to use.** Across the 9.9 MB rollout measured 2026-09-13 the
/// markers pair exactly: 50 `task_started` against 37 `task_complete` + 13 `turn_aborted` = 50.
/// Every started turn is closed by one of the two, so "the last marker is `task_started`" means a
/// turn is genuinely open and "the last marker is a close" means Codex is back at its prompt.
pub fn codex_turn(rollout: &Path) -> Option<(CodexTurn, Option<i64>)> {
    let mut file = File::open(rollout).ok()?;
    let mut end = file.metadata().ok()?.len();
    let mut boundary = Vec::new();

    while end > 0 {
        let start = end.saturating_sub(ROLLOUT_SCAN_CHUNK_BYTES);
        let amount = (end - start) as usize;
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = vec![0; amount];
        file.read_exact(&mut bytes).ok()?;
        bytes.extend_from_slice(&boundary);
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();

        // Reverse, so the first match is the last record. The first line is split at the chunk
        // boundary when start > 0; carry that line into the next earlier chunk before parsing it.
        for (index, line) in lines.iter().enumerate().rev() {
            if start > 0 && index == 0 {
                continue;
            }
            let line = line.trim();
            if !line.starts_with('{') {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let kind = value
                .get("payload")
                .and_then(|p| p.get("type"))
                .and_then(|t| t.as_str());
            let turn = match kind {
                Some("task_started") => CodexTurn::Open,
                Some(word @ ("task_complete" | "turn_aborted")) => {
                    CodexTurn::Closed(word.to_string())
                }
                _ => continue,
            };
            let ts = value
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(parse_iso_ms);
            return Some((turn, ts));
        }

        if start == 0 {
            break;
        }
        boundary = lines
            .first()
            .map(|line| line.as_bytes().to_vec())
            .unwrap_or_default();
        end = start;
    }
    None
}

/// Count child rollouts whose own turn is still open. Child rollouts remain separate evidence: this
/// helper only supplies the parent's derived delegation state and never folds their counters into
/// the parent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CodexSubagentFacts {
    pub active: u32,
}

pub fn codex_subagent_facts(root: &Path, parent_id: &str) -> CodexSubagentFacts {
    let mut files = Vec::new();
    collect_rollouts(root, &mut files);
    let mut facts = CodexSubagentFacts::default();
    for file in files {
        let Some(head) = read_head(&file.path) else {
            continue;
        };
        if head.parent_id.as_deref() != Some(parent_id) {
            continue;
        }
        if matches!(codex_turn(&file.path), Some((CodexTurn::Open, _))) {
            facts.active += 1;
        }
    }
    facts
}

/// Codex's [`WaitPolicy`]: the `PermissionRequest` hook for permission, the rollout's
/// `turn_aborted` marker for interruption.
///
/// **What it cannot see, stated plainly.** `request_user_input` — the model asking a question —
/// has **no hook event**; `updatedInput`, the field a reply would travel in, is reserved in
/// Codex's own schema and a hook that sends it fails closed (openai/codex#28969). So `question()`
/// is always `None`: a Codex question reads `Running`, and this type says so rather than inventing
/// a signal.
pub struct CodexWait<'a> {
    turn: Option<&'a CodexTurn>,
    /// The newest hook event for this thread, when the installed hook has fired one.
    hook_event: Option<&'a str>,
}

impl<'a> CodexWait<'a> {
    pub fn new(turn: Option<&'a CodexTurn>, hook_event: Option<&'a str>) -> Self {
        Self { turn, hook_event }
    }

    /// The turn state once the owner cases are set aside: open → running, closed → waiting, and no
    /// marker at all → unknown rather than a guess.
    pub fn turn_state(&self) -> LiveState {
        match self.turn {
            Some(CodexTurn::Open) => LiveState::Running,
            Some(CodexTurn::Closed(_)) => LiveState::Waiting,
            None => LiveState::Unknown,
        }
    }

    /// The engine's own word for the turn, carried into `raw_word`.
    pub fn turn_word(&self) -> Option<String> {
        match self.turn {
            Some(CodexTurn::Open) => Some("task_started".to_string()),
            Some(CodexTurn::Closed(word)) => Some(word.clone()),
            None => None,
        }
    }
}

impl WaitPolicy for CodexWait<'_> {
    fn permission(&self) -> Option<WaitSignal> {
        // The hook may only overrule a RUNNING reading: the event file is append-only, and a
        // `PermissionRequest` line from an earlier turn would otherwise resurrect a session Codex
        // has already finished.
        if self.hook_event == Some("PermissionRequest")
            && matches!(self.turn, Some(CodexTurn::Open))
        {
            return Some(
                WaitSignal::new(OwnerWait::Permission)
                    .word("PermissionRequest")
                    .because(
                        "Codex's PermissionRequest hook fired, which the rollout cannot record"
                            .to_string(),
                    ),
            );
        }
        None
    }

    fn question(&self) -> Option<WaitSignal> {
        // Codex publishes no question signal at all — see the type's note. `None` is the honest
        // answer, not a placeholder.
        None
    }

    fn interruption(&self) -> Option<WaitSignal> {
        if matches!(self.turn, Some(CodexTurn::Closed(word)) if word == "turn_aborted") {
            return Some(
                WaitSignal::new(OwnerWait::Interruption)
                    .word("turn_aborted")
                    .because(
                        "Codex's rollout records turn_aborted, so the turn was cut short"
                            .to_string(),
                    ),
            );
        }
        None
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]` as UTC epoch milliseconds.
///
/// Hand-rolled because the crate carries no date library and this is the only format it meets.
/// An unparsable timestamp is `None` — a missing duration, never a 1970 one.
fn parse_iso_ms(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if (bytes[10] != b'T' && bytes[10] != b' ') || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let year: i64 = value.get(0..4)?.parse().ok()?;
    let month: i64 = value.get(5..7)?.parse().ok()?;
    let day: i64 = value.get(8..10)?.parse().ok()?;
    let hour: i64 = value.get(11..13)?.parse().ok()?;
    let minute: i64 = value.get(14..16)?.parse().ok()?;
    let second: i64 = value.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut rest = &value[19..];
    let mut millis: i64 = 0;
    if let Some(after_dot) = rest.strip_prefix('.') {
        let digits: String = after_dot.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        rest = &after_dot[digits.len()..];
        let mut frac: String = digits.chars().take(3).collect();
        while frac.len() < 3 {
            frac.push('0');
        }
        millis = frac.parse().ok()?;
    }

    let offset_s = match rest.as_bytes().first() {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(sign) if *sign == b'+' || *sign == b'-' => {
            let digits: String = rest[1..].chars().filter(char::is_ascii_digit).collect();
            if digits.len() < 2 {
                return None;
            }
            let hours: i64 = digits.get(0..2)?.parse().ok()?;
            let minutes: i64 = digits.get(2..4).unwrap_or("0").parse().ok()?;
            let magnitude = hours * 3600 + minutes * 60;
            if *sign == b'-' {
                -magnitude
            } else {
                magnitude
            }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second - offset_s) * 1000 + millis)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    const TOKEN_SENTINEL: &str = "sk-codex-LEAKCANARY-0123456789";
    const SID_A: &str = "01a0963c-e907-7372-85be-d56b6669b13a";
    const SID_B: &str = "019ddfa6-69b5-7452-9ce7-61fcb606b9f7";

    /// A `CODEX_HOME`-shaped fixture tree. No environment variable is touched, so these tests are
    /// safe to run in parallel with each other and with anything else in the crate.
    struct Home {
        dir: TempDir,
    }

    impl Home {
        fn new() -> Self {
            let dir = TempDir::new().expect("temp dir");
            fs::create_dir_all(dir.path().join("sessions/2026/09/12")).expect("sessions dir");
            Self { dir }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }

        fn adapter(&self) -> CodexAdapter {
            CodexAdapter::with_home(self.dir.path())
        }

        /// Write one rollout. `stem` is the part after `rollout-`; the fixtures use the engine's
        /// own `<ISO-ts>-<uuid>` spelling so lexical order is chronological.
        fn rollout(&self, stem: &str, records: &[String]) -> PathBuf {
            let path = self
                .dir
                .path()
                .join("sessions/2026/09/12")
                .join(format!("rollout-{stem}.jsonl"));
            let mut body = records.join("\n");
            body.push('\n');
            fs::write(&path, body).expect("write rollout");
            path
        }

        /// Write a rollout whose mtime is strictly later than `after_ms`. The standard library
        /// cannot set an mtime, so the later file is rewritten until the filesystem agrees it is
        /// later — bounded, and true on the first try on APFS.
        fn rollout_after(&self, stem: &str, records: &[String], after_ms: i64) -> PathBuf {
            let path = self.rollout(stem, records);
            for _ in 0..50 {
                let meta = fs::metadata(&path).expect("metadata");
                if util::mtime_ms(&meta) > after_ms {
                    return path;
                }
                std::thread::sleep(std::time::Duration::from_millis(4));
                let mut body = records.join("\n");
                body.push('\n');
                fs::write(&path, body).expect("rewrite rollout");
            }
            panic!("filesystem never reported a later mtime");
        }

        fn session_index(&self, entries: &[(&str, &str)]) {
            let body: String = entries
                .iter()
                .map(|(id, name)| {
                    json!({"id": id, "thread_name": name, "updated_at": "2026-09-12T15:00:00Z"})
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(self.dir.path().join("session_index.jsonl"), body).expect("write index");
        }

        fn auth(&self, body: Value) {
            fs::write(self.dir.path().join("auth.json"), body.to_string()).expect("write auth");
        }
    }

    fn mtime_of(path: &Path) -> i64 {
        util::mtime_ms(&fs::metadata(path).expect("metadata"))
    }

    fn meta(id: &str, session_id: &str, ts: &str, cwd: &str) -> String {
        json!({
            "timestamp": ts,
            "type": "session_meta",
            "payload": {
                "id": id,
                "session_id": session_id,
                "timestamp": ts,
                "cwd": cwd,
                "cli_version": "0.149.1",
                "model_provider": "openai",
                "thread_source": "user",
                "git": {"branch": "master"}
            }
        })
        .to_string()
    }

    fn subagent_meta(id: &str, session_id: &str, thread_source: &str) -> String {
        json!({
            "timestamp": "2026-09-12T15:31:11.659Z",
            "type": "session_meta",
            "payload": {
                "id": id,
                "session_id": session_id,
                "timestamp": "2026-09-12T15:31:11.659Z",
                "cwd": "/tmp",
                "thread_source": thread_source
            }
        })
        .to_string()
    }

    #[test]
    fn an_open_child_turn_is_reported_as_active_for_its_parent() {
        let home = Home::new();
        let parent = "019ddfa6-69b5-7452-9ce7-61fcb606b9f7";
        let child = "019ddfa6-69b5-7452-9ce7-61fcb606b9f8";
        home.rollout(
            "2026-09-12T15-31-11-000000-019ddfa6-69b5-7452-9ce7-61fcb606b9f8",
            &[
                subagent_meta(child, parent, "subagent"),
                r#"{"timestamp":"2026-09-12T15:31:12.000Z","type":"event_msg","payload":{"type":"task_started"}}"#.into(),
            ],
        );

        assert_eq!(
            codex_subagent_facts(home.path().join("sessions").as_path(), parent).active,
            1
        );
    }

    fn user_event(message: &str, ts: &str) -> String {
        json!({
            "timestamp": ts,
            "type": "event_msg",
            "payload": {"type": "user_message", "message": message, "images": null}
        })
        .to_string()
    }

    fn user_item(body: &str, ts: &str) -> String {
        json!({
            "timestamp": ts,
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": body}]
            }
        })
        .to_string()
    }

    fn tool_item(kind: &str, ts: &str) -> String {
        json!({"timestamp": ts, "type": "response_item", "payload": {"type": kind}}).to_string()
    }

    fn token_count(ts: &str, input: u64, cached: u64, output: u64, reasoning: u64) -> String {
        json!({
            "timestamp": ts,
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": input,
                        "cached_input_tokens": cached,
                        "cache_write_input_tokens": 7,
                        "output_tokens": output,
                        "reasoning_output_tokens": reasoning,
                        "total_tokens": input + output
                    },
                    "last_token_usage": {"input_tokens": 1, "output_tokens": 1}
                }
            }
        })
        .to_string()
    }

    /// A `token_count` carrying rate limits. `primary`/`secondary` are whole slot values so a
    /// test can put the 300-minute window in either one, or null a slot out entirely.
    fn limits_event(primary: Value, secondary: Value, plan: Value, reached: Value) -> String {
        json!({
            "timestamp": "2026-09-12T15:40:00.000Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {"total_token_usage": {"input_tokens": 10, "output_tokens": 2}},
                "rate_limits": {
                    "limit_id": "codex",
                    "primary": primary,
                    "secondary": secondary,
                    "plan_type": plan,
                    "rate_limit_reached_type": reached
                }
            }
        })
        .to_string()
    }

    fn window(used_percent: f64, window_minutes: u64, resets_at: i64) -> Value {
        json!({
            "used_percent": used_percent,
            "window_minutes": window_minutes,
            "resets_at": resets_at
        })
    }

    // --------------------------------------------------------------------------------------- //

    #[test]
    fn two_rollouts_of_one_resumed_thread_collapse_into_one_row() {
        let home = Home::new();
        let earlier = home.rollout(
            "2026-09-12T10-29-29-01a0963c",
            &[
                meta(SID_A, SID_A, "2026-09-12T10:29:29.385Z", "/tmp"),
                user_event("first ask, the head file", "2026-09-12T10:29:30.000Z"),
            ],
        );
        let earlier_ms = mtime_of(&earlier);
        let later = home.rollout_after(
            "2026-09-12T18-00-00-01a0963c",
            &[
                meta(SID_A, SID_A, "2026-09-12T18:00:00.000Z", "/var"),
                user_event("the resume's own first ask", "2026-09-12T18:00:01.000Z"),
            ],
            earlier_ms,
        );
        let later_ms = mtime_of(&later);
        assert!(
            later_ms > earlier_ms,
            "the fixture needs two distinct mtimes"
        );

        let report = home.adapter().discover_sessions();
        assert!(report.problem.is_none());
        assert_eq!(
            report.sessions.len(),
            1,
            "a resume is the same session, not a second one"
        );
        let row = &report.sessions[0];
        assert_eq!(row.key.sid, SID_A);
        assert_eq!(
            row.title, "first ask, the head file",
            "head fields: the EARLIEST file's"
        );
        assert_eq!(row.cwd.as_deref(), Some(Path::new("/tmp")));
        assert_eq!(
            row.first_active_ms,
            parse_iso_ms("2026-09-12T10:29:29.385Z")
        );
        assert_eq!(
            row.last_active_ms, later_ms,
            "last_active_ms is the max mtime over the group"
        );
        assert_eq!(row.source.files, 2);
        assert_eq!(row.source.paths.len(), 2);
        let SourceSignature::Codex { files } = &row.source_signature else {
            panic!("a Codex row carries a Codex signature");
        };
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn a_subagent_rollout_is_not_a_session_by_either_marker() {
        let home = Home::new();
        home.rollout(
            "2026-09-12T10-00-00-parent",
            &[
                meta(SID_A, SID_A, "2026-09-12T10:00:00.000Z", "/tmp"),
                user_event("the real session", "2026-09-12T10:00:01.000Z"),
            ],
        );
        // Marker one: `session_id` names the PARENT thread while `id` names this one.
        home.rollout(
            "2026-09-12T10-05-00-childA",
            &[subagent_meta(
                "019ddfa9-bee8-79c3-ae56-fe22e1c4a6a2",
                SID_A,
                "user",
            )],
        );
        // Marker two: the engine says so outright.
        home.rollout(
            "2026-09-12T10-06-00-childB",
            &[subagent_meta(SID_B, SID_B, "subagent")],
        );

        let report = home.adapter().discover_sessions();
        let sids: Vec<&str> = report.sessions.iter().map(|s| s.key.sid.as_str()).collect();
        assert_eq!(sids, vec![SID_A], "only the parent thread is a session");
    }

    #[test]
    fn every_wrapper_kind_is_skipped_and_the_real_first_user_message_wins() {
        for wrapper in WRAPPERS {
            let home = Home::new();
            home.rollout(
                "2026-09-12T11-00-00-wrapped",
                &[
                    meta(SID_A, SID_A, "2026-09-12T11:00:00.000Z", "/tmp"),
                    user_item(
                        &format!("{wrapper}\nCLI scaffolding"),
                        "2026-09-12T11:00:01Z",
                    ),
                    user_item("port the codex adapter", "2026-09-12T11:00:02Z"),
                ],
            );
            let report = home.adapter().discover_sessions();
            assert_eq!(report.sessions.len(), 1, "{wrapper}");
            assert_eq!(
                report.sessions[0].title, "port the codex adapter",
                "{wrapper} was not skipped"
            );
        }
    }

    #[test]
    fn the_user_message_event_outranks_an_earlier_response_item() {
        let home = Home::new();
        // Measured on this Mac: the response_item copy is written FIRST, so file order would pick
        // the wrong one if the chain were "first candidate seen" rather than "best tier".
        home.rollout(
            "2026-09-12T11-00-00-order",
            &[
                meta(SID_A, SID_A, "2026-09-12T11:00:00.000Z", "/tmp"),
                user_item("the response_item copy", "2026-09-12T11:00:01Z"),
                user_event("the user_message event", "2026-09-12T11:00:02Z"),
            ],
        );
        let report = home.adapter().discover_sessions();
        assert_eq!(report.sessions[0].title, "the user_message event");
    }

    #[test]
    fn a_session_with_nothing_but_wrappers_is_untitled_not_mislabelled() {
        let home = Home::new();
        home.rollout(
            "2026-09-12T11-00-00-empty",
            &[
                meta(SID_A, SID_A, "2026-09-12T11:00:00.000Z", "/tmp"),
                user_item("<environment_context>\ncwd=/tmp", "2026-09-12T11:00:01Z"),
            ],
        );
        let report = home.adapter().discover_sessions();
        assert_eq!(report.sessions[0].title, Session::UNTITLED);
    }

    #[test]
    fn a_thread_name_from_the_session_index_lands_in_name_not_in_title() {
        let home = Home::new();
        home.rollout(
            "2026-09-12T12-00-00-named",
            &[
                meta(SID_A, SID_A, "2026-09-12T12:00:00.000Z", "/tmp"),
                user_event("what the owner actually asked", "2026-09-12T12:00:01Z"),
            ],
        );
        home.session_index(&[(SID_A, "Run e2e tests here"), (SID_B, "some other thread")]);

        let report = home.adapter().discover_sessions();
        let row = &report.sessions[0];
        assert_eq!(row.name.as_deref(), Some("Run e2e tests here"));
        assert_eq!(
            row.title, "what the owner actually asked",
            "the name never overwrites the title"
        );
    }

    #[test]
    fn a_hundred_kilobyte_filler_line_does_not_hide_the_last_token_count() {
        let home = Home::new();
        // 120 KB of filler on ONE line, written after the token_count. It is past the 64 KB first
        // tail window, so the backwards reader must grow its budget, and a capped line buffer in
        // the forward reader would truncate it.
        let filler = json!({
            "timestamp": "2026-09-12T13:00:02.000Z",
            "type": "response_item",
            "payload": {"type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": "x".repeat(120_000)}]}
        })
        .to_string();
        assert!(filler.len() > 100_000, "the filler line must exceed 100 KB");
        let path = home.rollout(
            "2026-09-12T13-00-00-filler",
            &[
                meta(SID_A, SID_A, "2026-09-12T13:00:00.000Z", "/tmp"),
                token_count("2026-09-12T13:00:01.000Z", 500, 100, 50, 20),
                limits_event(
                    window(6.0, 300, 1_789_348_833),
                    window(13.0, 10080, 1_789_823_201),
                    json!("team"),
                    Value::Null,
                ),
                filler,
            ],
        );

        let metrics = read_metrics(std::slice::from_ref(&path)).expect("counters");
        assert_eq!(
            metrics.api_calls, 2,
            "both token_count events survive the filler"
        );
        assert_eq!(
            metrics.input_tokens, 10,
            "the LAST token_count wins, past the filler"
        );

        // The same file through the capacity tail reader: the answer sits ~120 KB from EOF.
        let capacity = home.adapter().capacity_at(mtime_of(&path));
        assert!(
            capacity.supported,
            "the tail reader grew its window past the filler"
        );
        assert_eq!(capacity.windows.len(), 2);
    }

    #[test]
    fn a_half_written_last_record_does_not_hide_the_files_own_capacity_figure() {
        let home = Home::new();
        // What an actively-appending Codex leaves at EOF: a record carrying BOTH needles, cut
        // mid-JSON, with no trailing newline. The valid figure sits one line earlier in the SAME
        // rollout — and the owner's live session is the NEWEST file, so it is tried first. Before
        // the fix `.last()` took the truncated record, failed to parse it, and abandoned this
        // file for an older one's staler figure (or for a spurious unknown shape).
        let truncated = concat!(
            r#"{"timestamp":"2026-09-12T15:41:00.000Z","type":"event_msg","payload":"#,
            r#"{"type":"token_count","rate_limits":{"primary":{"used_perc"#
        );
        let path = home
            .path()
            .join("sessions/2026/09/12")
            .join("rollout-2026-09-12T15-40-00-live.jsonl");
        let body = format!(
            "{}\n{}\n{}",
            meta(SID_A, SID_A, "2026-09-12T15:40:00.000Z", "/tmp"),
            limits_event(
                window(6.0, 300, 1_789_348_833),
                window(13.0, 10080, 1_789_823_201),
                json!("team"),
                Value::Null,
            ),
            truncated
        );
        assert!(!body.ends_with('\n'), "the fixture must end mid-record");
        assert!(truncated.contains("\"token_count\"") && truncated.contains("\"rate_limits\""));
        fs::write(&path, body).expect("write rollout");

        let capacity = home.adapter().capacity_at(mtime_of(&path));
        assert!(
            capacity.supported,
            "the live writer's own allowance was skipped: {:?}",
            capacity.problem
        );
        assert_eq!(capacity.windows.len(), 2);
        assert_eq!(capacity.plan.as_deref(), Some("team"));
    }

    #[test]
    fn a_rollout_past_the_tail_budget_is_reported_unread_not_as_a_field_the_engine_omitted() {
        // Small file, covered end to end, stating no rate limits at all. The field really is
        // absent, and an unknown shape is the honest answer.
        let small = Home::new();
        let small_path = small.rollout(
            "2026-09-12T19-00-00-nolimits",
            &[
                meta(SID_A, SID_A, "2026-09-12T19:00:00.000Z", "/tmp"),
                token_count("2026-09-12T19:00:01.000Z", 100, 40, 10, 5),
            ],
        );
        let problem = small
            .adapter()
            .capacity_at(mtime_of(&small_path))
            .problem
            .expect("no figure anywhere is a stated problem");
        assert_eq!(problem.kind, ErrorKind::UnknownShape);

        // The same absence, in a rollout larger than the largest tail budget — eight rollouts on
        // this Mac are, the largest 17.4 MB. The first `len - 4 MB` bytes are never read, so
        // nothing here is evidence about Codex's schema and the error must not claim it is.
        let big = Home::new();
        let filler = json!({
            "timestamp": "2026-09-12T19:10:01.000Z",
            "type": "response_item",
            "payload": {"type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": "y".repeat(100_000)}]}
        })
        .to_string();
        let mut records = vec![meta(SID_B, SID_B, "2026-09-12T19:10:00.000Z", "/tmp")];
        records.extend(std::iter::repeat_n(filler, 45));
        let big_path = big.rollout("2026-09-12T19-10-00-huge", &records);
        let biggest_budget = *TAIL_BUDGETS.last().expect("a budget");
        assert!(
            fs::metadata(&big_path).expect("metadata").len() > biggest_budget,
            "the fixture must exceed the largest tail budget"
        );
        let problem = big
            .adapter()
            .capacity_at(mtime_of(&big_path))
            .problem
            .expect("a tail we could not exhaust is a stated problem");
        assert_eq!(
            problem.kind,
            ErrorKind::Io,
            "a budget we chose must not be reported as a field the engine did not write"
        );
        assert_eq!(
            problem.detail,
            ErrorDetail::Path {
                path: big_path.display().to_string()
            },
            "the error names the file it stopped short of"
        );
    }

    #[test]
    fn total_token_usage_comes_from_the_last_token_count_not_the_first() {
        let home = Home::new();
        let path = home.rollout(
            "2026-09-12T14-00-00-cumulative",
            &[
                meta(SID_A, SID_A, "2026-09-12T14:00:00.000Z", "/tmp"),
                token_count("2026-09-12T14:00:01.000Z", 100, 40, 10, 5),
                tool_item("function_call", "2026-09-12T14:00:02.000Z"),
                tool_item("custom_tool_call", "2026-09-12T14:00:03.000Z"),
                tool_item("local_shell_call", "2026-09-12T14:00:04.000Z"),
                tool_item("reasoning", "2026-09-12T14:00:05.000Z"),
                user_event("a turn", "2026-09-12T14:00:06.000Z"),
                token_count("2026-09-12T14:00:07.000Z", 900, 300, 80, 25),
            ],
        );
        let metrics = read_metrics(&[path]).expect("counters");
        assert_eq!(
            metrics.input_tokens, 600,
            "the last event's 900 input, less its 300 cached"
        );
        assert_eq!(metrics.cache_read, 300);
        assert_eq!(metrics.cache_write, 7);
        assert_eq!(metrics.output_tokens, 80);
        assert_eq!(metrics.reasoning_tokens, Some(25));
        assert_eq!(
            metrics.api_calls, 2,
            "api_calls counts the events, not the totals"
        );
        assert_eq!(
            metrics.tool_calls, 3,
            "only the three call kinds, not `reasoning`"
        );
        assert_eq!(metrics.user_turns, 1);
        assert_eq!(metrics.duration_ms, Some(7_000));
        assert_eq!(
            metrics.provider_cost_usd, None,
            "a subscription login has no stated price"
        );
    }

    #[test]
    fn a_window_is_matched_by_its_minutes_whichever_slot_carried_it() {
        let straight = Home::new();
        straight.rollout(
            "2026-09-12T15-00-00-straight",
            &[
                meta(SID_A, SID_A, "2026-09-12T15:00:00.000Z", "/tmp"),
                limits_event(
                    window(6.0, 300, 1_789_348_833),
                    window(13.0, 10080, 1_789_823_201),
                    json!("team"),
                    Value::Null,
                ),
            ],
        );
        let swapped = Home::new();
        swapped.rollout(
            "2026-09-12T15-00-00-swapped",
            &[
                meta(SID_A, SID_A, "2026-09-12T15:00:00.000Z", "/tmp"),
                limits_event(
                    window(13.0, 10080, 1_789_823_201),
                    window(6.0, 300, 1_789_348_833),
                    json!("team"),
                    Value::Null,
                ),
            ],
        );

        let now = util::now_ms();
        let mut a = straight.adapter().capacity_at(now).windows;
        let mut b = swapped.adapter().capacity_at(now).windows;
        a.sort_by_key(|w| w.window_minutes);
        b.sort_by_key(|w| w.window_minutes);
        assert_eq!(a, b, "the slot a window arrived in changes nothing");
        assert_eq!(a[0].name, CapacityWindowName::FiveHour);
        assert_eq!(a[0].used_pct, 6.0);
        // epoch SECONDS on the wire, milliseconds everywhere inside Pigeon.
        assert_eq!(a[0].resets_at_ms, Some(1_789_348_833_000));
        assert_eq!(a[1].name, CapacityWindowName::Weekly);
        assert_eq!(a[1].used_pct, 13.0);
    }

    #[test]
    fn an_unrecognised_window_is_dropped_rather_than_guessed_into_a_slot() {
        let home = Home::new();
        home.rollout(
            "2026-09-12T15-10-00-thirdcap",
            &[
                meta(SID_A, SID_A, "2026-09-12T15:10:00.000Z", "/tmp"),
                limits_event(
                    window(6.0, 300, 1_789_348_833),
                    window(99.0, 1440, 1_789_400_000),
                    json!("team"),
                    Value::Null,
                ),
            ],
        );
        let capacity = home.adapter().capacity_at(util::now_ms());
        assert!(capacity.supported);
        assert_eq!(
            capacity.windows.len(),
            1,
            "a daily cap we do not know is not a weekly one"
        );
        assert_eq!(capacity.windows[0].name, CapacityWindowName::FiveHour);
    }

    #[test]
    fn a_reading_older_than_an_hour_is_flagged_stale_and_still_shown() {
        let home = Home::new();
        let path = home.rollout(
            "2026-09-12T15-20-00-stale",
            &[
                meta(SID_A, SID_A, "2026-09-12T15:20:00.000Z", "/tmp"),
                limits_event(
                    window(6.0, 300, 1_789_348_833),
                    window(13.0, 10080, 1_789_823_201),
                    json!("team"),
                    Value::Null,
                ),
            ],
        );
        let file_ms = mtime_of(&path);

        let fresh = home.adapter().capacity_at(file_ms + 60_000);
        assert!(!fresh.stale);
        assert_eq!(fresh.source_age_s, Some(60));

        let stale = home.adapter().capacity_at(file_ms + 7_200_000);
        assert!(stale.stale, "two hours old is stale");
        assert_eq!(stale.source_age_s, Some(7_200));
        assert!(
            stale.supported,
            "stale is a FLAG, not an error — the figure is still shown"
        );
        assert_eq!(stale.windows.len(), 2);
        assert!(stale.problem.is_none());
    }

    #[test]
    fn a_reached_limit_surfaces_the_engines_own_word_and_draws_no_bars() {
        let home = Home::new();
        home.rollout(
            "2026-09-12T15-30-00-reached",
            &[
                meta(SID_A, SID_A, "2026-09-12T15:30:00.000Z", "/tmp"),
                limits_event(
                    Value::Null,
                    Value::Null,
                    json!("team"),
                    json!("workspace_member_credits_depleted"),
                ),
            ],
        );
        let capacity = home.adapter().capacity_at(util::now_ms());
        assert_eq!(
            capacity.reached_limit.as_deref(),
            Some("workspace_member_credits_depleted")
        );
        assert_eq!(capacity.plan.as_deref(), Some("team"));
        assert!(
            !capacity.supported,
            "a blocked account never draws the previous reading's bars"
        );
        assert!(capacity.windows.is_empty());
        let problem = capacity.problem.expect("a reached limit is reported");
        assert_eq!(
            problem.detail,
            ErrorDetail::Word {
                word: "workspace_member_credits_depleted".to_string()
            }
        );
    }

    #[test]
    fn a_rate_limit_slot_of_the_wrong_shape_fails_loud_instead_of_rendering() {
        let home = Home::new();
        home.rollout(
            "2026-09-12T15-40-00-drift",
            &[
                meta(SID_A, SID_A, "2026-09-12T15:40:00.000Z", "/tmp"),
                limits_event(
                    json!({"used_percent": "six", "window_minutes": 300}),
                    Value::Null,
                    json!("team"),
                    Value::Null,
                ),
            ],
        );
        let capacity = home.adapter().capacity_at(util::now_ms());
        assert!(!capacity.supported);
        assert!(
            capacity.windows.is_empty(),
            "never a number derived from a guessed schema"
        );
        let problem = capacity.problem.expect("drift is reported");
        assert_eq!(problem.kind, ErrorKind::UnknownShape);
    }

    #[test]
    fn no_token_count_at_all_is_an_absence_not_a_zero_bar() {
        let home = Home::new();
        let path = home.rollout(
            "2026-09-12T16-00-00-nocalls",
            &[
                meta(SID_A, SID_A, "2026-09-12T16:00:00.000Z", "/tmp"),
                user_event(
                    "opened codex and thought better of it",
                    "2026-09-12T16:00:01.000Z",
                ),
                tool_item("function_call", "2026-09-12T16:00:02.000Z"),
            ],
        );
        let metrics = read_metrics(&[path]).expect("a zero-call session still parses");
        assert_eq!(metrics.api_calls, 0);
        assert_eq!(metrics.input_tokens, 0);
        assert_eq!(
            metrics.reasoning_tokens, None,
            "no usage was stated, so none is reported"
        );
        assert_eq!(metrics.user_turns, 1, "what WAS proven is still reported");
        assert_eq!(metrics.tool_calls, 1);
        // The proof that this is an absence and not a rendered zero: every KPI is undefined,
        // because `ratio` returns None on a zero denominator. No bar can be drawn from None.
        let kpis = metrics.kpis();
        assert_eq!(kpis.context_per_call, None);
        assert_eq!(kpis.batching_ratio, None);
        assert_eq!(kpis.rewrite_ratio, None);
    }

    #[test]
    fn a_usage_field_of_the_wrong_type_fails_loud() {
        let home = Home::new();
        let path = home.rollout(
            "2026-09-12T16-10-00-badusage",
            &[
                meta(SID_A, SID_A, "2026-09-12T16:10:00.000Z", "/tmp"),
                json!({
                    "timestamp": "2026-09-12T16:10:01.000Z",
                    "type": "event_msg",
                    "payload": {"type": "token_count",
                                "info": {"total_token_usage": {"input_tokens": "lots"}}}
                })
                .to_string(),
            ],
        );
        let err = read_metrics(&[path]).expect_err("a string token count is drift");
        assert_eq!(err.kind, ErrorKind::UnknownShape);
    }

    #[test]
    fn a_token_count_without_an_info_object_still_counts_as_an_api_call() {
        // Measured 2026-09-13: 35 of 3 208 token_count events on this Mac carry no `info`.
        let home = Home::new();
        let path = home.rollout(
            "2026-09-12T16-20-00-noinfo",
            &[
                meta(SID_A, SID_A, "2026-09-12T16:20:00.000Z", "/tmp"),
                token_count("2026-09-12T16:20:01.000Z", 100, 40, 10, 5),
                json!({
                    "timestamp": "2026-09-12T16:20:02.000Z",
                    "type": "event_msg",
                    "payload": {"type": "token_count"}
                })
                .to_string(),
            ],
        );
        let metrics = read_metrics(&[path]).expect("counters");
        assert_eq!(metrics.api_calls, 2);
        assert_eq!(
            metrics.input_tokens, 60,
            "the last STATED usage stands: 100 less 40 cached"
        );
    }

    #[test]
    fn no_token_value_in_auth_json_reaches_identity_or_an_error() {
        let home = Home::new();
        home.auth(json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": TOKEN_SENTINEL,
            "tokens": {
                "id_token": TOKEN_SENTINEL,
                "access_token": TOKEN_SENTINEL,
                "refresh_token": TOKEN_SENTINEL,
                "account_id": "f982f0c4-e49c-44fe-a97a-c4f2dcf181f6"
            },
            "last_refresh": "2026-09-12T10:00:00.000Z"
        }));

        let identity = home.adapter().read_identity();
        assert!(identity.signed_in);
        assert_eq!(identity.mode.as_deref(), Some("chatgpt"));
        assert_eq!(
            identity.account_short.as_deref(),
            Some("f181f6"),
            "the LAST 6, never the id"
        );
        let rendered = format!("{identity:?}");
        assert!(!rendered.contains("LEAKCANARY"), "a token reached Identity");
        assert!(
            !rendered.contains("f982f0c4"),
            "the account id is bounded to its tail"
        );
        if let Some(problem) = &identity.problem {
            let json = serde_json::to_string(problem).expect("serializes");
            assert!(
                !json.contains("LEAKCANARY"),
                "a token reached an EngineError"
            );
        }

        // The same file, unparsable: the error must be built from the fields we wanted, never
        // from serde's message — which quotes the offending input.
        let broken = Home::new();
        fs::write(
            broken.path().join("auth.json"),
            format!("{{\"auth_mode\": \"chatgpt\", \"tokens\": {TOKEN_SENTINEL}"),
        )
        .expect("write");
        let identity = broken.adapter().read_identity();
        let problem = identity.problem.expect("a broken auth.json is reported");
        let json = serde_json::to_string(&problem).expect("serializes");
        assert!(
            !json.contains("LEAKCANARY"),
            "serde's own message leaked into an EngineError"
        );
        assert_eq!(problem.kind, ErrorKind::UnknownShape);
    }

    #[test]
    fn a_missing_auth_file_is_a_stated_absence_not_a_failure() {
        let home = Home::new();
        let identity = home.adapter().read_identity();
        assert!(!identity.signed_in);
        assert_eq!(
            identity.problem.map(|p| p.kind),
            Some(ErrorKind::NoCredential)
        );
    }

    #[test]
    fn a_missing_sessions_root_is_reported_and_blanks_nothing_else() {
        let dir = TempDir::new().expect("temp dir");
        let adapter = CodexAdapter::with_home(dir.path());
        let report = adapter.discover_sessions();
        assert!(report.sessions.is_empty());
        assert_eq!(report.problem.map(|p| p.kind), Some(ErrorKind::RootMissing));
        assert_eq!(
            adapter.read_capacity().problem.map(|p| p.kind),
            Some(ErrorKind::RootMissing)
        );
    }

    #[test]
    fn timestamps_parse_to_utc_epoch_milliseconds() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_iso_ms("2026-09-12T15:31:11.659Z"),
            Some(1_789_227_071_659)
        );
        // The same instant written with an offset lands on the same millisecond.
        assert_eq!(
            parse_iso_ms("2026-09-12T17:31:11.659+02:00"),
            Some(1_789_227_071_659)
        );
        assert_eq!(
            parse_iso_ms("2026-09-12T15:31:11Z"),
            Some(1_789_227_071_000)
        );
        assert_eq!(parse_iso_ms("not a timestamp"), None);
        assert_eq!(parse_iso_ms(""), None);
    }

    #[test]
    fn a_session_whose_folder_is_gone_is_not_resumable() {
        let home = Home::new();
        home.rollout(
            "2026-09-12T17-00-00-gone",
            &[meta(
                SID_A,
                SID_A,
                "2026-09-12T17:00:00.000Z",
                "/no/such/folder/here",
            )],
        );
        let report = home.adapter().discover_sessions();
        let row = &report.sessions[0];
        assert!(!row.resumable);
        assert_eq!(
            row.resume_blocked_reason,
            Some(ResumeBlockedReason::WorkingDirectoryMissing)
        );
    }

    #[test]
    fn a_file_with_no_user_message_events_falls_back_to_its_filtered_user_items() {
        let home = Home::new();
        // This file states its turns as events AND carries the response_item copies of the same
        // words — the shape of 50 of the 72 rollouts here. The fallback must stay out of its way.
        let with_events = home.rollout(
            "2026-09-12T18-00-00-events",
            &[
                meta(SID_A, SID_A, "2026-09-12T18:00:00.000Z", "/tmp"),
                user_item("<environment_context>\ncwd=/tmp", "2026-09-12T18:00:01Z"),
                user_item("the first ask, restated", "2026-09-12T18:00:02Z"),
                user_event("the first ask", "2026-09-12T18:00:03Z"),
                user_item("the second ask, restated", "2026-09-12T18:00:04Z"),
                user_event("the second ask", "2026-09-12T18:00:05Z"),
            ],
        );
        let metrics = read_metrics(std::slice::from_ref(&with_events)).expect("counters");
        assert_eq!(
            metrics.user_turns, 2,
            "events win outright; the copies are not added to them"
        );

        // The paginated-history shape: no `user_message` event anywhere. 22 of 72 rollouts on
        // this Mac look like this, including all three of the newest sessions, and counting 0
        // turns for them would be a wrong number rather than a stated absence.
        let without_events = home.rollout(
            "2026-09-12T19-00-00-paginated",
            &[
                meta(SID_A, SID_A, "2026-09-12T19:00:00.000Z", "/tmp"),
                user_item("<environment_context>\ncwd=/tmp", "2026-09-12T19:00:01Z"),
                user_item("# AGENTS.md\ncontributor guide", "2026-09-12T19:00:02Z"),
                user_item("the only real ask", "2026-09-12T19:00:03Z"),
                user_item("a follow-up", "2026-09-12T19:00:04Z"),
            ],
        );
        let metrics = read_metrics(std::slice::from_ref(&without_events)).expect("counters");
        assert_eq!(
            metrics.user_turns, 2,
            "the two wrappers are scaffolding, not turns"
        );

        // Both files as one resumed thread: the rule is per FILE, so the first file's events
        // never suppress the second file's fallback.
        let metrics = read_metrics(&[with_events, without_events]).expect("counters");
        assert_eq!(metrics.user_turns, 4);
    }

    #[test]
    fn input_tokens_reports_the_uncached_remainder_so_it_sums_with_a_claude_row() {
        let home = Home::new();
        // The real figures from the newest session on this Mac, 2026-09-13.
        let path = home.rollout(
            "2026-09-12T20-00-00-overlap",
            &[
                meta(SID_A, SID_A, "2026-09-12T20:00:00.000Z", "/tmp"),
                token_count(
                    "2026-09-12T20:00:01.000Z",
                    12_208_937,
                    11_544_064,
                    76_863,
                    17_140,
                ),
            ],
        );
        let metrics = read_metrics(std::slice::from_ref(&path)).expect("counters");
        // Codex states `input_tokens` as a SUPERSET containing `cached_input_tokens`; Claude
        // states the two as disjoint. `Metrics::input_tokens` means "uncached input" across the
        // product, and the project card sums a Codex row with a Claude row — so the overlap is
        // removed here, once, rather than double-counting the cached prompt in every total.
        assert_eq!(
            metrics.input_tokens, 664_873,
            "12,208,937 less the 11,544,064 cached"
        );
        assert_eq!(
            metrics.cache_read, 11_544_064,
            "the cached prompt is reported once, here"
        );
        assert_eq!(
            metrics.input_tokens + metrics.cache_read,
            12_208_937,
            "the two together are the wire's input figure, counted exactly once"
        );
    }

    #[test]
    fn a_cached_figure_larger_than_input_fails_loud_rather_than_underflowing() {
        let home = Home::new();
        let path = home.rollout(
            "2026-09-12T20-10-00-impossible",
            &[
                meta(SID_A, SID_A, "2026-09-12T20:10:00.000Z", "/tmp"),
                token_count("2026-09-12T20:10:01.000Z", 100, 900, 10, 5),
            ],
        );
        // Nothing here saturates a wrong number into a plausible one: the relationship the
        // subtraction rests on has changed, and that is news.
        let err = read_metrics(std::slice::from_ref(&path)).expect_err("cached > input is drift");
        assert_eq!(err.kind, ErrorKind::UnknownShape);
    }

    /// Real data, this Mac, 2026-09-13 (Codex 0.149.1, 72 rollout files, 68 logical sessions).
    /// Returns early rather than failing where Codex has never run.
    #[test]
    fn real_codex_sessions_on_this_machine_group_into_whole_uuid_rows() {
        let adapter = CodexAdapter::new();
        if !adapter.sessions_root().is_dir() {
            return;
        }
        let report = adapter.discover_sessions();
        assert!(
            report.problem.is_none(),
            "real discovery reported a problem"
        );
        assert!(
            !report.sessions.is_empty(),
            "a populated ~/.codex/sessions produced no rows"
        );

        let mut seen: BTreeSet<String> = BTreeSet::new();
        for row in &report.sessions {
            assert_eq!(
                row.key.sid.len(),
                36,
                "a sid was truncated: {}",
                row.key.sid
            );
            assert!(
                util::is_uuid(&row.key.sid),
                "not a whole uuid: {}",
                row.key.sid
            );
            assert!(
                seen.insert(row.key.sid.clone()),
                "two rows share {}",
                row.key.sid
            );
            assert!(row.last_active_ms > 0);
            assert!(!row.title.is_empty());
            assert!(row.source.files >= 1);
        }
    }

    // -- Turn marker and timestamps, moved here with the reader ---------------------------------

    #[test]
    fn a_codex_turn_is_read_from_the_tail_and_no_marker_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rollout.jsonl");
        // Padding ahead of the markers, so the tail window is doing real work.
        let mut body = String::new();
        for _ in 0..400 {
            body.push_str("{\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\"}}\n");
        }
        body.push_str(
            "{\"timestamp\":\"2026-09-13T21:00:00.000Z\",\
             \"payload\":{\"type\":\"task_started\"}}\n",
        );
        std::fs::write(&path, &body).expect("write");
        assert_eq!(
            codex_turn(&path),
            Some((CodexTurn::Open, parse_iso_ms("2026-09-13T21:00:00.000Z")))
        );

        body.push_str(
            "{\"timestamp\":\"2026-09-13T21:04:17.468Z\",\
             \"payload\":{\"type\":\"task_complete\"}}\n",
        );
        std::fs::write(&path, &body).expect("write");
        let (turn, ts) = codex_turn(&path).expect("a closed turn");
        assert_eq!(turn, CodexTurn::Closed("task_complete".into()));
        assert_eq!(ts, parse_iso_ms("2026-09-13T21:04:17.468Z"));

        // No marker at all is Unknown, not a guess.
        std::fs::write(&path, b"{\"payload\":{\"type\":\"reasoning\"}}\n").expect("write");
        assert_eq!(codex_turn(&path), None);
    }

    #[test]
    fn a_codex_turn_marker_survives_more_than_one_megabyte_of_following_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rollout.jsonl");
        let mut body = String::from(
            "{\"timestamp\":\"2026-09-14T21:00:00.000Z\",\"payload\":{\"type\":\"task_started\"}}\n",
        );
        for _ in 0..30_000 {
            body.push_str(
                "{\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\",\"text\":\"output\"}}\n",
            );
        }
        std::fs::write(&path, body).expect("write");

        assert_eq!(
            codex_turn(&path),
            Some((CodexTurn::Open, parse_iso_ms("2026-09-14T21:00:00.000Z")))
        );
    }

    // -- The three owner cases, through the shared policy ---------------------------------------

    #[test]
    fn a_permission_hook_over_an_open_turn_is_the_permission_case() {
        let open = CodexTurn::Open;
        let wait = CodexWait::new(Some(&open), Some("PermissionRequest"));
        let signal = wait.owner_wait().expect("the hook parks it on the owner");
        assert_eq!(signal.case, OwnerWait::Permission);
        assert_eq!(signal.case.state(), LiveState::NeedsYou);
        assert_eq!(signal.raw_word.as_deref(), Some("PermissionRequest"));
    }

    #[test]
    fn a_stale_permission_hook_cannot_reopen_a_closed_turn() {
        let closed = CodexTurn::Closed("task_complete".into());
        let wait = CodexWait::new(Some(&closed), Some("PermissionRequest"));
        assert!(
            wait.permission().is_none(),
            "an append-only hook line from an earlier turn must not resurrect a finished one"
        );
        assert_eq!(wait.turn_state(), LiveState::Waiting);
    }

    #[test]
    fn codex_has_no_question_signal_and_says_so_rather_than_inventing_one() {
        let open = CodexTurn::Open;
        let wait = CodexWait::new(Some(&open), Some("PreToolUse"));
        assert!(
            wait.question().is_none(),
            "request_user_input has no hook event (openai/codex#28969)"
        );
        assert!(wait.owner_wait().is_none());
        assert_eq!(wait.turn_state(), LiveState::Running);
    }

    #[test]
    fn a_turn_aborted_marker_is_the_interruption_case_and_reads_waiting() {
        let aborted = CodexTurn::Closed("turn_aborted".into());
        let signal = CodexWait::new(Some(&aborted), None)
            .owner_wait()
            .expect("an abort is a wait");
        assert_eq!(signal.case, OwnerWait::Interruption);
        assert_eq!(signal.case.state(), LiveState::Waiting);
        assert_eq!(signal.raw_word.as_deref(), Some("turn_aborted"));
    }

    #[test]
    fn no_turn_marker_is_unknown_rather_than_a_guess() {
        let wait = CodexWait::new(None, None);
        assert!(wait.owner_wait().is_none());
        assert_eq!(wait.turn_state(), LiveState::Unknown);
        assert_eq!(wait.turn_word(), None);
    }
}
