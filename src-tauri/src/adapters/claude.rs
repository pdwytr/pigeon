//! The Claude Code adapter: `~/.claude` on disk, and the OAuth usage endpoint.
//!
//! Three source facts shape everything below, each verified read-only on this Mac (2026-09-12,
//! re-measured 2026-09-13: 18 project directories, 51 transcripts under `~/.claude/projects`,
//! 21,033 records, CLI 2.1.128 – 2.1.270):
//!
//! 1. **A session is a depth-1 `<uuid>.jsonl`.** The subdirectories beside it are subagent trees
//!    (named by the parent session's uuid) and `memory/`; a `sessions-index.json` sits there too.
//!    Only a UUID stem is a session, which is why [`crate::util::is_uuid`] gates discovery.
//! 2. **The head of the file answers discovery.** `cwd` first appears by line 8 and the first real
//!    user text by line 11 in 51/51 files, so nothing here reads a whole transcript to draw a row
//!    — the largest on this machine is 14.6 MB.
//! 3. **The encoded directory name is never decoded.** `-Users-khalid-Documents-Projects-feather`
//!    is lossy (a hyphen in a real folder name is indistinguishable from a separator); Studio
//!    carries that bug as backlog #26. The `cwd` the engine itself recorded is the only truth.
//!
//! **The fold is a port, not a re-invention.** [`fold_usage`] carries Demo Studio's frozen
//! counting rules over verbatim (`model.fold_usage_max` + `parser._build_api_calls`): dedupe API
//! calls by `message.id`, and within one id take the per-key elementwise MAX. `output_tokens` is a
//! streaming counter, so summing the logged records double-counts 2–6× and taking the first
//! occurrence under-counts. Changing any of it is an ADR-level decision in both products.
//!
//! That rule also absorbs, for free, the shape that broke Studio twice in September 2026: a forked
//! subagent's transcript restates its parent's dispatching assistant record byte for byte — same
//! `message.id`, same `usage`, same `tool_use` block (Studio `bd8ed9bb`, `a6b5dd98`). Deduping by
//! id counts it once. Pigeon reads only the depth-1 transcript today, so the cross-file half of
//! Studio's fix has nothing to bite on here; what matters is that nothing below defeats the
//! within-file dedupe by counting records instead of ids.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{ProviderAdapter, ProviderSessionReport, SessionCandidate};
use crate::api::errors::{EngineError, ErrorDetail, ErrorKind, Secret};
use crate::domain::{
    Capacity, CapacityWindow, CapacityWindowName, Diagnostics, FileSignature, Identity, Metrics,
    ProviderId, ResumeBlockedReason, Session, SessionKey, SourceSignature, SourceSummary,
};
use crate::util::{is_uuid, mtime_ms, now_ms, tidy_title};

/// The one provider this file speaks for.
const PROVIDER: ProviderId = ProviderId::ClaudeCode;

/// The head scan's two bounds, whichever comes first. Discovery needs `cwd` (line 8 at worst) and
/// a title (line 11 at worst) and nothing else, so reading further buys nothing and costs 14 MB on
/// the largest transcript here. A session whose head holds no user text yet is untitled, not
/// missing.
const HEAD_MAX_LINES: usize = 400;
const HEAD_MAX_BYTES: u64 = 1024 * 1024;

/// The four usage categories the frozen rules operate on, in the engine's own spelling and in the
/// order they are folded into [`Metrics`] (input, output, cache read, cache write). Real `usage`
/// objects carry several more —
/// `service_tier`, `server_tool_use`, `cache_creation`, `iterations`, `speed`,
/// `output_tokens_details` — and none of them is part of any frozen KPI, so they are read by
/// nothing here rather than guessed into a counter.
const USAGE_KEYS: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];

/// Record types this adapter knows about — the 25 Studio's `records.KNOWN_RECORD_TYPES` holds,
/// which is the vocabulary four fail-loud waves there have already paid for. A type outside this
/// set is *counted in [`Diagnostics`]*, never silently dropped and never guessed at: the engine
/// adds types (`fork-context-ref` and `continued-in` both arrived in September 2026), and the only
/// honest report of one we do not know is that we do not know it.
const KNOWN_RECORD_TYPES: [&str; 25] = [
    "agent-name",
    "ai-title",
    "artifact-autoreact-ledger",
    "artifact-comment-monitor",
    "assistant",
    "atis-latch",
    "attachment",
    "bridge-session",
    "continued-in",
    "cost-state",
    "custom-title",
    "file-history-delta",
    "file-history-snapshot",
    "fork-context-ref",
    "frame-link",
    "history-suppression",
    "last-prompt",
    "mode",
    "permission-mode",
    "pr-link",
    "queue-operation",
    "relocated",
    "system",
    "user",
    "worktree-state",
];

/// The macOS Keychain service Claude Code stores the stock login under, read out of the 2.1.267
/// binary. `security`'s exit status 44 is `errSecItemNotFound` — the ordinary state of a Mac that
/// never signed in, and therefore the same stated absence a missing file is.
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const KEYCHAIN_ITEM_NOT_FOUND: i32 = 44;
/// Bounded because a Keychain may decide to *ask*: an ACL prompt must not park the caller forever.
const KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Short on purpose: a capacity reading that is ten seconds late is a reading nobody wanted.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The two window keys the endpoint publishes, and which allowance each is. The body also carries
/// `seven_day_opus`, `seven_day_oauth_apps` and a dozen other keys; a window Pigeon cannot name
/// by [`CapacityWindowName`] is not drawn.
/// The field names an unknown shape names back. Constants because three call sites state them and
/// a drifted spelling in one of them would be a lie about what was looked for.
const WINDOW_FIELDS: [&str; 2] = ["five_hour", "seven_day"];
const TOKEN_FIELD: [&str; 1] = ["claudeAiOauth.accessToken"];

const WINDOW_KEYS: [(&str, CapacityWindowName); 2] = [
    ("five_hour", CapacityWindowName::FiveHour),
    ("seven_day", CapacityWindowName::Weekly),
];

// ------------------------------------------------------------------------------------------- //
// the adapter
// ------------------------------------------------------------------------------------------- //

/// What the Keychain had to say. Three answers, because they mean three different things: an item,
/// no item at all (an absence), or a Keychain that stood in the way (a failure).
#[derive(Debug)]
pub enum KeychainAnswer {
    /// The item's secret. Not a [`Secret`] yet — the item is a JSON *document* that has to be
    /// parsed before the token inside it can be isolated; see [`ClaudeAdapter::load_login`].
    Item(String),
    Absent,
    Refused,
}

/// Reads one Keychain service. A function pointer rather than a boxed closure so the adapter stays
/// `Send + Sync` and cheap to clone — and so tests can substitute a reader and never touch the
/// developer's own Keychain, which is the only reason this is a seam at all.
pub type KeychainReader = fn(&str) -> KeychainAnswer;

pub struct ClaudeAdapter {
    /// The owner's home. `~/.claude/` holds the transcripts and the credentials file; `~/.claude.json`
    /// sits beside it, not inside it.
    home: PathBuf,
    keychain: KeychainReader,
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self {
            // An account with no resolvable home gets an empty path, which fails as a stated
            // `RootMissing` on the first read rather than panicking at construction.
            home: dirs::home_dir().unwrap_or_default(),
            keychain: read_macos_keychain,
        }
    }

    /// A adapter rooted at a different home. Tests use it; so would a future managed-config-home
    /// reader (`CLAUDE_CONFIG_DIR`), which is why it takes a home rather than each path.
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            keychain: read_macos_keychain,
        }
    }

    /// Substitute the Keychain reader. Every test that can reach the credential path uses this:
    /// a suite that opens the developer's real login is a suite that can pop an ACL dialog.
    pub fn with_keychain(mut self, keychain: KeychainReader) -> Self {
        self.keychain = keychain;
        self
    }

    /// `~/.claude/projects` — one directory per encoded cwd, transcripts directly inside it.
    fn projects_dir(&self) -> PathBuf {
        self.home.join(".claude").join("projects")
    }

    /// `~/.claude.json` — the engine's own config, which carries `oauthAccount`.
    fn config_file(&self) -> PathBuf {
        self.home.join(".claude.json")
    }

    /// `~/.claude/.credentials.json` — present on Linux/Windows and on some Macs; on this Mac the
    /// stock login lives in the Keychain instead and this file does not exist.
    fn credentials_file(&self) -> PathBuf {
        self.home.join(".claude").join(".credentials.json")
    }
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderAdapter for ClaudeAdapter {
    fn provider(&self) -> ProviderId {
        PROVIDER
    }

    fn discover_sessions(&self) -> ProviderSessionReport {
        let root = self.projects_dir();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            // A machine that has never run Claude Code is an ordinary machine. The path is the
            // whole point of the message, so it travels; nothing else about the error does.
            Err(err) => {
                return ProviderSessionReport::problem(EngineError::from_io(PROVIDER, &err, &root))
            }
        };

        let mut sessions = Vec::new();
        for project in entries.flatten() {
            let dir = project.path();
            let Ok(files) = std::fs::read_dir(&dir) else {
                // One unreadable project directory is not the engine failing. The rest still list.
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(OsStr::to_str) != Some("jsonl") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(OsStr::to_str) else {
                    continue;
                };
                // Depth 1 with a UUID stem, and nothing else: `memory/`, the subagent tree named
                // after the session uuid, and `sessions-index.json` all fail one of these.
                if !is_uuid(stem) {
                    continue;
                }
                let Ok(meta) = std::fs::metadata(&path) else {
                    continue;
                };
                if !meta.is_file() {
                    continue;
                }
                sessions.push(self.candidate(&dir, &path, stem, &meta));
            }
        }
        ProviderSessionReport {
            sessions,
            problem: None,
        }
    }

    fn read_identity(&self) -> Identity {
        let read_at_ms = now_ms();
        let path = self.config_file();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                let problem = EngineError::from_io(PROVIDER, &err, &path);
                return Identity::absent(PROVIDER, read_at_ms, Some(problem));
            }
        };
        // The file is ~130 KB of project history on this Mac. It is parsed whole because there is
        // no streaming path in serde_json worth the complexity here, and then *dropped*: the six
        // fields below are all that survives this function.
        let Ok(config) = serde_json::from_slice::<Value>(&bytes) else {
            return Identity::absent(
                PROVIDER,
                read_at_ms,
                Some(EngineError::unknown_shape(PROVIDER, &["oauthAccount"])),
            );
        };
        let Some(account) = config.get("oauthAccount").and_then(Value::as_object) else {
            // Signed out is an ordinary state, not a problem to report.
            return Identity::absent(PROVIDER, read_at_ms, None);
        };
        let field = |key: &str| {
            account
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };

        Identity {
            provider: PROVIDER,
            signed_in: true,
            // The email is the label the owner recognises; `displayName` is the fallback for an
            // account shape that states no address.
            label: field("emailAddress").or_else(|| field("displayName")),
            organization: field("organizationName"),
            plan: field("userRateLimitTier"),
            tier: field("seatTier"),
            // Claude Code has one sign-in route, so there is no `auth_mode` to state (Codex has).
            mode: None,
            // A bounded tail, for telling two logins apart. Never the whole id.
            account_short: field("accountUuid").map(|uuid| {
                let tail: String = uuid.chars().rev().take(6).collect();
                tail.chars().rev().collect()
            }),
            providers: None,
            read_at_ms,
            problem: None,
        }
    }

    /// The trait method is synchronous and the reading is an HTTP call, so this drives the async
    /// half on a runtime of its own and blocks on it.
    ///
    /// **Why that is correct rather than merely convenient:** the accounts service calls every
    /// adapter's `read_capacity` on a plain `std::thread` of its own (`services/accounts.rs`,
    /// `off_runtime`), which carries no runtime context at all. `spawn_blocking` is **not** enough
    /// and was tried: a blocking worker is still *in* a runtime context — that is exactly what
    /// makes `Handle::current()` work there — and the built app died on this very call with
    /// *"Cannot start a runtime from within a runtime"* before the account strip could render.
    /// Do not move it back. The runtime built here is current-thread, owned by this call, and
    /// dropped with it, so it never touches the app's runtime or its worker threads. The
    /// alternative — a `reqwest::blocking` client — is not available: `blocking` is not one of the
    /// features `Cargo.toml` declares, and it would spawn a background runtime of its own anyway.
    ///
    /// **The one way to hold it wrong**, and tokio's rule rather than this adapter's: calling it
    /// from *inside* an async task panics, because that thread is already driving a runtime. An
    /// async caller wants [`ClaudeAdapter::fetch_capacity`] directly, which is why that is public.
    fn read_capacity(&self) -> Capacity {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(self.fetch_capacity()),
            // A runtime that will not build is an I/O failure (no reactor, no threads); it says
            // nothing about the endpoint, so it must not be reported as if it did.
            Err(_) => {
                Capacity::problem(PROVIDER, now_ms(), EngineError::of(PROVIDER, ErrorKind::Io))
            }
        }
    }
}

// ------------------------------------------------------------------------------------------- //
// discovery
// ------------------------------------------------------------------------------------------- //

impl ClaudeAdapter {
    /// One transcript as a draft session. Reads the file's head only; the fold that produces the
    /// numbers is a separate, later pass ([`fold_usage`]) run by the metrics service.
    fn candidate(
        &self,
        dir: &Path,
        path: &Path,
        sid: &str,
        meta: &std::fs::Metadata,
    ) -> SessionCandidate {
        let head = read_head(path).unwrap_or_else(|_| {
            // An unreadable head still leaves a real session: its id and its clock are facts of
            // the directory listing. Dropping the row would hide a session the owner has.
            let mut head = Head::default();
            head.diagnostics.note("unreadable-head");
            head
        });

        let cwd = head.cwd.clone();
        let (resumable, resume_blocked_reason) = match &cwd {
            None => (false, Some(ResumeBlockedReason::MissingWorkingDirectory)),
            Some(dir) if !dir.is_dir() => {
                (false, Some(ResumeBlockedReason::WorkingDirectoryMissing))
            }
            Some(_) => (true, None),
        };

        SessionCandidate {
            key: SessionKey::new(PROVIDER, sid),
            cwd,
            title: head.title.unwrap_or_else(|| Session::UNTITLED.to_string()),
            // The engine's own title where it stated one inside the head bound (see
            // `Head::complete`). `None` is a real answer: a session the engine has not named yet
            // has no name, and the row falls back to `title`.
            name: head.name,
            git_branch: head.git_branch,
            first_active_ms: head.first_ts_ms,
            // The engine appends to this file for the life of the session, so its mtime is when the
            // session was last active — no record has to be read to know it.
            last_active_ms: mtime_ms(meta),
            // Claude Code writes no close marker; a session is only ever "not touched since".
            closed_at_ms: None,
            resumable,
            resume_blocked_reason,
            source: SourceSummary::single(path.to_path_buf()),
            diagnostics: head.diagnostics,
            source_signature: SourceSignature::Claude {
                main: FileSignature {
                    path: path.to_path_buf(),
                    size: meta.len(),
                    mtime_ms: mtime_ms(meta),
                },
                // The subagent tree beside the transcript, named by the session's own uuid. Its
                // mtime moves when a subagent writes, which a cache keyed on the main file alone
                // would not see.
                sidecar_mtime_ms: std::fs::metadata(dir.join(sid)).ok().map(|m| mtime_ms(&m)),
            },
        }
    }
}

/// What the bounded head pass found.
#[derive(Debug, Default)]
struct Head {
    cwd: Option<PathBuf>,
    title: Option<String>,
    /// The engine's own short title for the session, from an `ai-title` record.
    name: Option<String>,
    git_branch: Option<String>,
    first_ts_ms: Option<i64>,
    /// Types met in the head window and not recognised. A *subset* by construction — the pass
    /// stops early by design — so it is an early warning of drift, not a whole-file tally. The
    /// fold produces that one.
    diagnostics: Diagnostics,
}

impl Head {
    /// Everything the head pass is looking for. The early exit waits on `name` as well as on cwd
    /// and title, which is why the pass does not usually stop at line 11: `ai-title` is written
    /// once the engine has named the conversation, and it lands as late as **line 198** in the 51
    /// transcripts measured here. The bound is unchanged — a file whose `ai-title` never arrives
    /// inside it simply has no name, which is what `Option` already says.
    fn complete(&self) -> bool {
        self.cwd.is_some() && self.title.is_some() && self.name.is_some()
    }
}

/// Read a transcript's head: cwd, first user text, the engine's own title, git branch, first
/// timestamp.
fn read_head(path: &Path) -> Result<Head, EngineError> {
    let file = File::open(path).map_err(|err| EngineError::from_io(PROVIDER, &err, path))?;
    Ok(read_head_from(&mut BufReader::new(file)))
}

/// The head pass itself, over any reader. Split out so a test can assert the one property that
/// matters about it and cannot be seen from the outside: **how many bytes it takes**. Given the
/// reader, a test reads what is left afterwards and subtracts.
fn read_head_from<R: BufRead>(reader: &mut R) -> Head {
    let mut head = Head::default();
    let mut buffer: Vec<u8> = Vec::with_capacity(4096);
    let mut bytes: u64 = 0;

    for _ in 0..HEAD_MAX_LINES {
        buffer.clear();
        // **The byte bound is enforced on the way IN, per line.** A bare `read_until` is
        // unbounded and can only be checked once a whole record is already in memory, which makes
        // the declared ceiling a description of what we noticed rather than of what we read.
        // Measured on this Mac 2026-09-13: four transcripts hold a single record larger than this
        // bound and the largest such record is 1,308,288 bytes — so the ceiling was already
        // breached on real data, and a transcript whose FIRST record was 14 MB would have been
        // read whole by the pass whose entire purpose is to stop exactly that.
        let remaining = HEAD_MAX_BYTES.saturating_sub(bytes);
        if remaining == 0 {
            break;
        }
        // `read_until` rather than `read_line`: a transcript is UTF-8 in practice, but an invalid
        // byte would make `read_line` fail and leave the reader at an undefined position, which
        // turns one bad byte into a session with no title.
        match reader
            .by_ref()
            .take(remaining)
            .read_until(b'\n', &mut buffer)
        {
            Ok(0) => break,
            Ok(n) => bytes += n as u64,
            Err(_) => break,
        }
        let Ok(record) = serde_json::from_slice::<Value>(&buffer) else {
            // A record cut short by OUR ceiling, not by the engine. Neither drift nor corruption,
            // and there is nothing further this pass is allowed to read anyway.
            if bytes >= HEAD_MAX_BYTES {
                break;
            }
            // The engine appends while we read, so the LAST line of a live session is routinely
            // half-written. That is not drift and must not be reported as a record type — but the
            // justification covers only the last line, and every earlier one that fails to parse
            // is a genuinely corrupt record. Dropping those silently is how a hole in a transcript
            // reads as an ordinary session. `fill_buf` tells the two apart without consuming a
            // byte: an empty buffer at this point means the line just read really was the last.
            let at_eof = reader
                .fill_buf()
                .map(|chunk| chunk.is_empty())
                .unwrap_or(false);
            if !at_eof {
                head.diagnostics.note("(unparsable record)");
            }
            continue;
        };

        if head.first_ts_ms.is_none() {
            head.first_ts_ms = record
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(iso8601_to_ms);
        }
        if head.cwd.is_none() {
            // The engine's own `cwd`, never the encoded directory name (see the module note).
            if let Some(cwd) = record
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
            {
                head.cwd = Some(PathBuf::from(cwd));
            }
        }
        if head.git_branch.is_none() {
            // Rides on the same records `cwd` does, but not on all of them — the third attachment
            // record of a real session carries `cwd` and no branch — so it is read on its own.
            head.git_branch = record
                .get("gitBranch")
                .and_then(Value::as_str)
                .filter(|b| !b.is_empty())
                .map(str::to_string);
        }

        match record.get("type").and_then(Value::as_str) {
            Some("user") => {
                if let UserRecord::Turn { text: Some(text) } = classify_user(&record) {
                    if head.title.is_none() {
                        head.title = Some(tidy_title(&text, 120));
                    }
                }
            }
            // The engine's own short title — "Kubernetes SAS service manager" rather than the
            // owner's first paragraph — and a better row label for it. Present in 48 of the 51
            // transcripts here; the first one wins, because a session can be re-titled and the
            // earliest statement is the one the head pass can afford to reach.
            Some("ai-title") => {
                if head.name.is_none() {
                    let stated = record.get("aiTitle").and_then(Value::as_str);
                    let stated = stated.filter(|title| !title.trim().is_empty());
                    head.name = stated.map(|title| tidy_title(title, 120));
                }
            }
            Some(other) => {
                if !KNOWN_RECORD_TYPES.contains(&other) {
                    head.diagnostics.note(other);
                }
            }
            None => head.diagnostics.note("(untyped record)"),
        }

        if head.complete() || bytes >= HEAD_MAX_BYTES {
            break;
        }
    }
    head
}

/// What one `type: "user"` record is. The engine writes three different things under that name and
/// only one of them is the owner speaking.
#[derive(Debug, PartialEq, Eq)]
enum UserRecord {
    /// A real owner turn, with whatever text it stated.
    Turn { text: Option<String> },
    /// Harness plumbing: a tool result being fed back, the engine's own post-compaction summary,
    /// or a task notification. None of the three is a turn and none is a title.
    Plumbing,
}

/// Classify one `user` record, exactly as Studio's `_scan_transcript` does.
///
/// The three exclusions are each load-bearing and each was earned there:
/// `tool_result`-bearing records are the harness feeding itself (Studio's `has_tool_result`);
/// `isCompactSummary` marks ~19 KB of the engine's own prose about a conversation it just
/// discarded, credited to the compaction rather than the owner (ADR-0052); and an
/// `origin.kind == "task-notification"` record is a synthesized message about a background task.
/// A record that is a turn but states no text still counts as a turn — Studio's `else` arm — it
/// simply cannot supply a title.
fn classify_user(record: &Value) -> UserRecord {
    if record.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
        return UserRecord::Plumbing;
    }
    let origin_kind = record
        .get("origin")
        .and_then(|o| o.get("kind"))
        .and_then(Value::as_str);
    if origin_kind == Some("task-notification") {
        return UserRecord::Plumbing;
    }
    match record.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(text)) => {
            let text = text.trim();
            UserRecord::Turn {
                text: (!text.is_empty()).then(|| text.to_string()),
            }
        }
        Some(Value::Array(blocks)) => {
            let mut parts: Vec<&str> = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_result") => return UserRecord::Plumbing,
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            parts.push(text);
                        }
                    }
                    _ => {}
                }
            }
            let joined = parts.join(" ");
            let joined = joined.trim();
            UserRecord::Turn {
                text: (!joined.is_empty()).then(|| joined.to_string()),
            }
        }
        _ => UserRecord::Turn { text: None },
    }
}

// ------------------------------------------------------------------------------------------- //
// the frozen fold
// ------------------------------------------------------------------------------------------- //

/// A fold's numbers plus what it could not name. [`fold_usage`] is the metrics service's door;
/// this is for anything that also wants the drift tally.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fold {
    pub metrics: Metrics,
    pub diagnostics: Diagnostics,
}

/// One transcript's counters, under the frozen counting rules.
///
/// **The rule, stated once:** API calls are deduped by `message.id`, and within one id every usage
/// key takes the elementwise MAX across the records sharing it. `output_tokens` is a *streaming
/// counter* that grows line by line as one message is written, so summing the records double-counts
/// 2–6× (measured in Studio) and reading the first occurrence under-counts. `tool_calls` obey the
/// same identity rule: a `tool_use` block restated on a later line of the same streamed message is
/// one call, not two.
///
/// Fails loud in exactly two places, both of them "this number would be a guess": an `assistant`
/// record with no `message.id` (there is no dedupe key, so any count is invented) and a `usage`
/// object in which none of the four frozen keys appears (the schema moved). An unrecognised record
/// *type* is not one of them — it is counted in [`Fold::diagnostics`] and the rest of the file is
/// still read, because a new type the engine added says nothing about the records we do understand.
pub fn fold_usage(path: &Path) -> Result<Metrics, EngineError> {
    fold_detailed(path).map(|fold| fold.metrics)
}

/// [`fold_usage`], keeping the diagnostics.
pub fn fold_detailed(path: &Path) -> Result<Fold, EngineError> {
    let file = File::open(path).map_err(|err| EngineError::from_io(PROVIDER, &err, path))?;
    let mut reader = BufReader::new(file);
    let mut buffer: Vec<u8> = Vec::with_capacity(8192);

    // message.id -> the elementwise MAX so far, in USAGE_KEYS order. The map's size IS `api_calls`.
    let mut per_call: HashMap<String, [u64; 4]> = HashMap::new();
    // message.id -> the MAX stated `thinking_tokens`. A SEPARATE map, and that is the whole point:
    // an id only enters it when a record actually stated the field, so a session where nothing
    // states it stays empty and reports `None` rather than `Some(0)`. Those are different facts.
    let mut per_call_thinking: HashMap<String, u64> = HashMap::new();
    // Every `tool_use` id seen. Engine-minted and unique per call, so one set answers both
    // "restated on another line of this message" and "restated in another message".
    let mut tool_ids: HashSet<String> = HashSet::new();
    let mut user_turns: u64 = 0;
    let mut first_ts_ms: Option<i64> = None;
    let mut last_ts_ms: Option<i64> = None;
    let mut diagnostics = Diagnostics::default();

    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => return Err(EngineError::from_io(PROVIDER, &err, path)),
        }
        if buffer.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(&buffer) else {
            // A live session's last line is routinely half-written. Stated, not silent, and not
            // fatal: the 30,000 records before it are still the owner's real numbers.
            //
            // **The same distinction the head pass draws, for the same reason.** That excuse
            // belongs to the LAST line only; an unparsable line with more file after it is a hole
            // in the record, and tallying the two under one name hides the one that matters. No
            // counter moves either way — this is what the fold could not read, not what it
            // counted — so the frozen rules are untouched and only the label is honest now.
            let at_eof = reader
                .fill_buf()
                .map(|chunk| chunk.is_empty())
                .unwrap_or(false);
            diagnostics.note(if at_eof {
                "(half-written last line)"
            } else {
                "(unparsable record)"
            });
            continue;
        };

        // File order, not min/max: the first stamp the file states and the last one, which is what
        // Studio's `first_ts`/`last_ts` mean.
        if let Some(ms) = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(iso8601_to_ms)
        {
            first_ts_ms.get_or_insert(ms);
            last_ts_ms = Some(ms);
        }

        match record.get("type").and_then(Value::as_str) {
            Some("assistant") => {
                let Some(message) = record.get("message").and_then(Value::as_object) else {
                    return Err(EngineError::unknown_shape(PROVIDER, &["message"]));
                };
                let stated = message.get("id").and_then(Value::as_str);
                let Some(mid) = stated.filter(|id| !id.is_empty()) else {
                    // The dedupe key itself. Without it every rule below is a guess about which
                    // records belong to one call, so this is the fail-loud boundary.
                    return Err(EngineError::unknown_shape(PROVIDER, &["message.id"]));
                };
                if let Some(usage) = message.get("usage").and_then(Value::as_object) {
                    let logged = read_usage(usage)?;
                    let snapshot = per_call.entry(mid.to_string()).or_insert([0; 4]);
                    for (slot, value) in snapshot.iter_mut().zip(logged.iter()) {
                        *slot = (*slot).max(*value);
                    }
                    match read_thinking(usage) {
                        // The same frozen MAX rule, for the same reason: it rides inside the
                        // streaming `usage` payload and grows across the records of one id.
                        Thinking::Stated(stated) => {
                            let slot = per_call_thinking.entry(mid.to_string()).or_insert(0);
                            *slot = (*slot).max(stated);
                        }
                        Thinking::Unstated => {}
                        // Stated but not a number. Not fatal, deliberately: this is a displayed
                        // field and not one of the four frozen counters, so refusing the whole
                        // fold over it would trade a real session's six numbers for one. The
                        // figure stays unstated and the drift is on the record.
                        Thinking::Drifted => diagnostics.note("(thinking_tokens is not a number)"),
                    }
                }
                if let Some(blocks) = message.get("content").and_then(Value::as_array) {
                    for block in blocks {
                        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                            continue;
                        }
                        match block
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.is_empty())
                        {
                            Some(id) => {
                                tool_ids.insert(id.to_string());
                            }
                            // No id means no identity, so it cannot be deduped and must not be
                            // counted as if it could. Stated rather than dropped.
                            None => diagnostics.note("(tool_use without id)"),
                        }
                    }
                }
            }
            Some("user") => {
                if classify_user(&record) != UserRecord::Plumbing {
                    user_turns += 1;
                }
            }
            Some(other) => {
                if !KNOWN_RECORD_TYPES.contains(&other) {
                    diagnostics.note(other);
                }
            }
            None => diagnostics.note("(untyped record)"),
        }
    }

    let mut metrics = Metrics {
        api_calls: per_call.len() as u64,
        tool_calls: tool_ids.len() as u64,
        user_turns,
        // A span needs both ends; a clock that ran backwards is not a duration, so it is an
        // absence rather than a negative number or a zero.
        duration_ms: match (first_ts_ms, last_ts_ms) {
            (Some(first), Some(last)) if last >= first => Some((last - first) as u64),
            _ => None,
        },
        // **Pigeon's own addition, not a ported rule** — no frozen Studio counting rule reads
        // `usage.output_tokens_details.thinking_tokens`, and Studio's own KPIs are unaffected by
        // it. It is here because Codex fills `reasoning_tokens` from its `reasoning_output_tokens`
        // and a project card sums the two engines: leaving Claude's empty would make a mixed
        // project's reasoning total silently mean "the Codex part only". Like Codex's, it is a
        // SUBSET of `output_tokens` and is never added to them — a separate displayed field.
        reasoning_tokens: (!per_call_thinking.is_empty()).then(|| {
            per_call_thinking
                .values()
                .fold(0u64, |total, v| total.saturating_add(*v))
        }),
        // A subscription login. A per-token price here would be fiction, and the field exists to
        // carry an engine's OWN figure (OpenCode's), never one Pigeon invented.
        provider_cost_usd: None,
        ..Default::default()
    };
    for snapshot in per_call.values() {
        metrics.input_tokens = metrics.input_tokens.saturating_add(snapshot[0]);
        metrics.output_tokens = metrics.output_tokens.saturating_add(snapshot[1]);
        metrics.cache_read = metrics.cache_read.saturating_add(snapshot[2]);
        metrics.cache_write = metrics.cache_write.saturating_add(snapshot[3]);
    }
    Ok(Fold {
        metrics,
        diagnostics,
    })
}

/// One logged `usage` payload as the four frozen categories, in [`USAGE_KEYS`] order.
///
/// Absent or `null` reads as 0 — a call that read no cache legitimately states no
/// `cache_read_input_tokens`, and Studio's `fold_usage_max` has always read it that way. A key that
/// is present but is *not* a number is drift, and so is a `usage` object in which none of the four
/// appears at all: both would otherwise render a plausible number out of a schema that moved.
fn read_usage(usage: &serde_json::Map<String, Value>) -> Result<[u64; 4], EngineError> {
    let mut values = [0u64; 4];
    let mut recognised = 0;
    for (slot, key) in values.iter_mut().zip(USAGE_KEYS.iter()) {
        match usage.get(*key) {
            None => continue,
            Some(Value::Null) => recognised += 1,
            Some(value) => {
                let Some(number) = value.as_u64() else {
                    return Err(EngineError::unknown_shape(PROVIDER, &[key]));
                };
                *slot = number;
                recognised += 1;
            }
        }
    }
    if recognised == 0 {
        return Err(EngineError::unknown_shape(PROVIDER, &USAGE_KEYS));
    }
    Ok(values)
}

/// What one `usage` payload said about thinking tokens. Three answers, because unstated and zero
/// are different statements and a drifted shape is a third thing again.
enum Thinking {
    Stated(u64),
    Unstated,
    Drifted,
}

/// `usage.output_tokens_details.thinking_tokens`, as stated.
///
/// A stated `0` IS a measurement and is kept; an absent key, an absent `output_tokens_details`, or
/// a `null` is *unstated* and is never promoted to a number — older records and older CLI builds
/// state nothing here, and a 0 would be Pigeon asserting "this call did no thinking" about a
/// record that said no such thing.
fn read_thinking(usage: &serde_json::Map<String, Value>) -> Thinking {
    let Some(details) = usage.get("output_tokens_details") else {
        return Thinking::Unstated;
    };
    match details {
        Value::Null => Thinking::Unstated,
        Value::Object(details) => match details.get("thinking_tokens") {
            None | Some(Value::Null) => Thinking::Unstated,
            Some(value) => match value.as_u64() {
                Some(stated) => Thinking::Stated(stated),
                None => Thinking::Drifted,
            },
        },
        _ => Thinking::Drifted,
    }
}

// ------------------------------------------------------------------------------------------- //
// ISO-8601, by hand
// ------------------------------------------------------------------------------------------- //

/// `YYYY-MM-DDTHH:MM:SS[.fff…][Z|±HH[:MM]]` as UTC epoch milliseconds, or `None`.
///
/// By hand because this build declares no date crate and needs exactly one format: every one of
/// the 21,033 timestamps in the 51 transcripts on this Mac (2026-09-12) is
/// `YYYY-MM-DDTHH:MM:SS.sssZ`, 24 characters. The offset forms are accepted anyway — `+00:00` is
/// what Studio writes and what the usage endpoint's `resets_at` may carry — and anything else
/// answers `None` rather than a plausible wrong instant.
///
/// A stamp with no zone at all is read as UTC. That is the engine's own convention (it writes `Z`),
/// and the alternative — this machine's local zone — would silently shift every duration by the
/// owner's offset.
pub fn iso8601_to_ms(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    if !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let (year, month, day) = (digits(b, 0, 4)?, digits(b, 5, 2)?, digits(b, 8, 2)?);
    let (hour, minute, second) = (digits(b, 11, 2)?, digits(b, 14, 2)?, digits(b, 17, 2)?);
    let dated = (1..=12).contains(&month) && (1..=31).contains(&day);
    if !dated || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let mut at = 19;
    let mut millis = 0i64;
    if b.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while at < b.len() && b[at].is_ascii_digit() {
            at += 1;
        }
        if at == start {
            return None;
        }
        // Only the first three digits are milliseconds; a finer stamp is truncated, not rounded,
        // because a rounded stamp can land in the next second and make a duration negative.
        for offset in 0..3 {
            let digit = if start + offset < at {
                i64::from(b[start + offset] - b'0')
            } else {
                0
            };
            millis = millis * 10 + digit;
        }
    }

    let zone_seconds = match b.get(at) {
        None => 0,
        Some(b'Z' | b'z') if at + 1 == b.len() => 0,
        Some(&sign) if sign == b'+' || sign == b'-' => {
            let zone_hour = digits(b, at + 1, 2)?;
            let colon = b.get(at + 3) == Some(&b':');
            let zone_minute = if colon {
                digits(b, at + 4, 2)?
            } else {
                digits(b, at + 3, 2)?
            };
            let magnitude = zone_hour * 3600 + zone_minute * 60;
            if sign == b'-' {
                -magnitude
            } else {
                magnitude
            }
        }
        Some(_) => return None,
    };

    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    Some((seconds - zone_seconds) * 1000 + millis)
}

/// `len` ASCII digits at `at`, or `None` if they are not all digits (or run off the end).
fn digits(bytes: &[u8], at: usize, len: usize) -> Option<i64> {
    let slice = bytes.get(at..at + len)?;
    let mut value = 0i64;
    for byte in slice {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i64::from(byte - b'0');
    }
    Some(value)
}

/// Days since 1970-01-01 for a proleptic-Gregorian date — Howard Hinnant's `days_from_civil`,
/// which is the standard closed form and handles leap years and centuries without a table.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

// ------------------------------------------------------------------------------------------- //
// capacity: the credential, the endpoint, the two windows
// ------------------------------------------------------------------------------------------- //

/// The stored login, reduced to what one request needs. The token's whole lifetime is this struct:
/// loaded, put in one header, dropped.
struct Login {
    token: Secret,
    /// `subscriptionType` — the engine's own word for the plan the windows belong to.
    plan: Option<String>,
}

impl ClaudeAdapter {
    /// Claude Code's two allowance windows, or a stated reason there are none.
    ///
    /// The async half of [`ProviderAdapter::read_capacity`], public because the accounts service
    /// already has a runtime and should not pay for a second one.
    pub async fn fetch_capacity(&self) -> Capacity {
        let read_at_ms = now_ms();
        let login = match self.load_login() {
            Ok(login) => login,
            Err(error) => return Capacity::problem(PROVIDER, read_at_ms, error),
        };
        let plan = login.plan.clone();

        let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
            Ok(client) => client,
            Err(_) => return transport_problem(read_at_ms),
        };
        // **The only `Secret::expose` in this file** — grep for it to audit the token's reach.
        // Three lines, and then the token is gone from everything that outlives them: the scratch
        // `bearer` string is dropped as soon as the header holds it, `login` is dropped as soon as
        // the request holds the header, and the header value is marked sensitive so an HTTP/2
        // implementation will not put it in a shared header table. From there the bytes exist only
        // inside the request that is about to be spent.
        let bearer = format!("Bearer {}", login.token.expose());
        let mut authorization = match reqwest::header::HeaderValue::from_str(&bearer) {
            Ok(value) => value,
            Err(_) => {
                return Capacity::problem(
                    PROVIDER,
                    read_at_ms,
                    EngineError::unknown_shape(PROVIDER, &TOKEN_FIELD),
                )
            }
        };
        drop(bearer);
        authorization.set_sensitive(true);
        let request = client
            .get(USAGE_URL)
            .header(reqwest::header::AUTHORIZATION, authorization)
            .header("anthropic-beta", OAUTH_BETA)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header(reqwest::header::ACCEPT, "application/json")
            .build();
        drop(login);

        let request = match request {
            Ok(request) => request,
            Err(_) => return transport_problem(read_at_ms),
        };
        // **Never `format!("{e}")` on a reqwest error**: its `Display` walks the source chain and
        // can print the URL, and a URL is one query parameter away from being a credential. What it
        // is asked for instead is its *type* — a status if it has one, transport otherwise.
        let response = match client.execute(request).await {
            Ok(response) => response,
            Err(error) => {
                let problem = match error.status() {
                    Some(status) => http_status_error(status.as_u16()),
                    None => EngineError::of(PROVIDER, ErrorKind::Transport),
                };
                return Capacity::problem(PROVIDER, read_at_ms, problem);
            }
        };

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Capacity::problem(PROVIDER, read_at_ms, http_status_error(status));
        }
        let Ok(body) = response.text().await else {
            // The status arrived and the body did not: the failure is transport, not the endpoint.
            return transport_problem(read_at_ms);
        };
        let Ok(body) = serde_json::from_str::<Value>(&body) else {
            return window_shape_problem(read_at_ms);
        };
        capacity_from_body(&body, plan, read_at_ms)
    }

    /// `(token, plan)` or the reason there is none. **The file first, the Keychain only in its
    /// absence**: the file is the carrier on every platform but macOS, and reading it spends no
    /// subprocess. Every absence here is an ordinary state of an ordinary machine.
    fn load_login(&self) -> Result<Login, EngineError> {
        let path = self.credentials_file();
        let document = if path.exists() {
            match std::fs::read(&path) {
                Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                    Ok(value) => value,
                    Err(_) => return Err(EngineError::unknown_shape(PROVIDER, &TOKEN_FIELD)),
                },
                Err(err) => return Err(EngineError::from_io(PROVIDER, &err, &path)),
            }
        } else {
            match (self.keychain)(KEYCHAIN_SERVICE) {
                // The item is the same JSON document the file would have held, so it has to be
                // parsed before the token inside it can be isolated into a `Secret`. The blob lives
                // in this expression and dies with it.
                KeychainAnswer::Item(blob) => match serde_json::from_str::<Value>(&blob) {
                    Ok(value) => value,
                    Err(_) => return Err(EngineError::unknown_shape(PROVIDER, &TOKEN_FIELD)),
                },
                KeychainAnswer::Absent => {
                    return Err(EngineError::of(PROVIDER, ErrorKind::NoCredential))
                }
                // The Keychain itself stood in the way. `security`'s stderr names the item and the
                // keychain file; it is never read, so there is nothing here it could carry.
                KeychainAnswer::Refused => return Err(EngineError::of(PROVIDER, ErrorKind::Io)),
            }
        };

        let oauth = document.get("claudeAiOauth").and_then(Value::as_object);
        let token = oauth
            .and_then(|o| o.get("accessToken"))
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty());
        let Some(token) = token else {
            // Not an error and not a missing credential: an API-key login is a working machine
            // configured a different way, and the 5-hour and weekly windows are a subscription
            // concept that does not apply to it.
            return Err(EngineError::of(PROVIDER, ErrorKind::Unsupported));
        };
        Ok(Login {
            token: Secret::new(token),
            plan: oauth
                .and_then(|o| o.get("subscriptionType"))
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
                .map(str::to_string),
        })
    }
}

/// A transport failure as a stated capacity: the request never reached the endpoint, so nothing
/// here is a claim about the endpoint or about the login.
fn transport_problem(read_at_ms: i64) -> Capacity {
    Capacity::problem(
        PROVIDER,
        read_at_ms,
        EngineError::of(PROVIDER, ErrorKind::Transport),
    )
}

/// The endpoint answered something this adapter cannot read as an allowance. Fail loud: never a
/// number out of a schema that moved.
fn window_shape_problem(read_at_ms: i64) -> Capacity {
    Capacity::problem(
        PROVIDER,
        read_at_ms,
        EngineError::unknown_shape(PROVIDER, &WINDOW_FIELDS),
    )
}

/// 401/403 mean the login was presented and refused; 429 and everything else mean the endpoint
/// answered about itself. The body is never read into any of them — a refusal body from an account
/// endpoint can name the account, the organisation, or the token's own id.
fn http_status_error(status: u16) -> EngineError {
    match status {
        401 | 403 => EngineError::of(PROVIDER, ErrorKind::CredentialRefused),
        code => EngineError::with(
            PROVIDER,
            ErrorKind::HttpStatus,
            ErrorDetail::Status { code },
        ),
    }
}

/// The endpoint's body as two windows. Pure, so the rules below are testable without a network.
///
/// **A `null` bucket renders ABSENT, never 0%.** The live body carries a dozen nulls
/// (`seven_day_opus`, `seven_day_oauth_apps`, …), and a weekly bar drawn empty because a field went
/// missing is the worst lie this surface could tell — it reads as "none of your allowance is used".
///
/// **A key that is present but the wrong shape fails loud**, as does a body with neither window
/// key. The endpoint is unsupported and undocumented, so a rename is a *when*, not an *if*, and the
/// only honest rendering of a field we can no longer read is that we can no longer read it.
fn capacity_from_body(body: &Value, plan: Option<String>, read_at_ms: i64) -> Capacity {
    let Some(object) = body.as_object() else {
        return window_shape_problem(read_at_ms);
    };
    if WINDOW_KEYS
        .iter()
        .all(|(key, _)| !object.contains_key(*key))
    {
        return window_shape_problem(read_at_ms);
    }

    let mut windows = Vec::new();
    let mut drift: Vec<String> = Vec::new();
    for (key, name) in WINDOW_KEYS {
        match object.get(key) {
            // Absent and explicitly null are the same statement: this window has nothing to say.
            None | Some(Value::Null) => continue,
            Some(Value::Object(bucket)) => {
                // `as_f64` answers `None` for a JSON bool, which is the case worth naming: a field
                // that flipped from a percentage to a flag would otherwise read as 0% or 1%.
                let Some(used_pct) = bucket.get("utilization").and_then(Value::as_f64) else {
                    drift.push(format!("{key}.utilization"));
                    continue;
                };
                windows.push(CapacityWindow {
                    name,
                    window_minutes: name.minutes(),
                    used_pct,
                    resets_at_ms: bucket
                        .get("resets_at")
                        .and_then(Value::as_str)
                        .and_then(iso8601_to_ms),
                });
            }
            Some(_) => drift.push(key.to_string()),
        }
    }

    if !drift.is_empty() {
        return Capacity::problem(
            PROVIDER,
            read_at_ms,
            EngineError::with(
                PROVIDER,
                ErrorKind::UnknownShape,
                ErrorDetail::Fields { fields: drift },
            ),
        );
    }
    Capacity {
        provider: PROVIDER,
        // The endpoint answered, so the engine does publish an allowance — even on a body whose
        // every bucket was null. `supported: false` is for an engine that has no such concept.
        supported: true,
        windows,
        plan,
        stale: false,
        // The figure was read live from the endpoint, so there is no source age to state. A number
        // here would be about this call's own latency, which is not what the field means.
        source_age_s: None,
        // Codex names a limit it has reached; Claude's body states no such word, so none is quoted.
        reached_limit: None,
        read_at_ms,
        problem: None,
    }
}

/// The stock login out of the macOS login Keychain — the call Claude Code itself makes to read it
/// back (`security find-generic-password -s <service> -w`), and nothing else.
///
/// **Inert off darwin and inert when `security` does not resolve**: both answer `Absent` without
/// spawning, so a machine with no credential pays nothing for an absence it already had. Contained
/// (argument list, no shell, `stdin` closed), bounded (a Keychain may decide to *ask*, and an ACL
/// dialog must not park the caller), and quiet: `stderr` goes to `/dev/null`, so the one place a
/// keychain path or item name could enter this process never opens.
fn read_macos_keychain(service: &str) -> KeychainAnswer {
    if !cfg!(target_os = "macos") {
        return KeychainAnswer::Absent;
    }
    let Ok(mut child) = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", service, "-w"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return KeychainAnswer::Absent;
    };

    let deadline = Instant::now() + KEYCHAIN_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return KeychainAnswer::Refused;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => return KeychainAnswer::Refused,
        }
    };
    // Safe to read after exit: the secret is ~1 KB, far inside the pipe buffer, so the child was
    // never blocked on a reader that had not arrived.
    let Ok(output) = child.wait_with_output() else {
        return KeychainAnswer::Refused;
    };
    match status.code() {
        // `errSecItemNotFound` — a Mac that has never signed into Claude Code. An absence.
        Some(KEYCHAIN_ITEM_NOT_FOUND) => KeychainAnswer::Absent,
        Some(0) => {
            let item = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if item.is_empty() {
                KeychainAnswer::Absent
            } else {
                KeychainAnswer::Item(item)
            }
        }
        _ => KeychainAnswer::Refused,
    }
}

// ------------------------------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{MetricBasis, MetricState};

    /// A token that is not a token, planted where a careless `map_err` or `Debug` would put one.
    const SENTINEL: &str = "sk-ant-oat01-LEAKCANARY-0123456789";
    const SID: &str = "4b2f0c16-9d31-4a6e-8f52-0a1b2c3d4e5f";

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("claude")
            .join(name)
    }

    fn write_lines(path: &Path, lines: &[&str]) -> PathBuf {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture directory");
        }
        std::fs::write(path, format!("{}\n", lines.join("\n"))).expect("fixture written");
        path.to_path_buf()
    }

    /// A Keychain that is never opened. Every test that can reach the credential path uses it.
    fn no_keychain(_service: &str) -> KeychainAnswer {
        KeychainAnswer::Absent
    }

    // --------------------------------------------------------------------------------------- //
    // discovery: title, cwd, what is and is not a session
    // --------------------------------------------------------------------------------------- //

    #[test]
    fn a_title_comes_from_a_string_content_or_from_the_text_parts_of_a_list() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let plain = write_lines(
            &temp.path().join("plain.jsonl"),
            &[
                r#"{"type":"attachment","cwd":"/w","timestamp":"2026-09-12T10:00:00.000Z"}"#,
                r#"{"type":"user","message":{"content":"  Port   the\nfold  "}}"#,
            ],
        );
        assert_eq!(
            read_head(&plain).expect("head").title.as_deref(),
            Some("Port the fold")
        );

        let parts = write_lines(
            &temp.path().join("parts.jsonl"),
            &[
                r#"{"type":"attachment","cwd":"/w","timestamp":"2026-09-12T10:00:00.000Z"}"#,
                concat!(
                    r#"{"type":"user","message":{"content":[{"type":"image","source":{}},"#,
                    r#"{"type":"text","text":"Read the parser"},"#,
                    r#"{"type":"text","text":"then port it"}]}}"#
                ),
            ],
        );
        let title = read_head(&parts).expect("head").title;
        assert_eq!(title.as_deref(), Some("Read the parser then port it"));
    }

    #[test]
    fn a_tool_result_carrier_and_a_compact_summary_are_not_the_title() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = write_lines(
            &temp.path().join("skips.jsonl"),
            &[
                r#"{"type":"attachment","cwd":"/w","timestamp":"2026-09-12T10:00:00.000Z"}"#,
                // Harness plumbing: the tool result being fed back, with text beside it.
                concat!(
                    r#"{"type":"user","message":{"content":[{"type":"text","text":"NOT A TITLE"},"#,
                    r#"{"type":"tool_result","tool_use_id":"toolu_1","content":"..."}]}}"#
                ),
                // The engine's own post-compaction prose, credited to the compaction.
                r#"{"type":"user","isCompactSummary":true,"message":{"content":"ALSO NOT A TITLE"}}"#,
                // A background task's notification, synthesized under `user`.
                concat!(
                    r#"{"type":"user","origin":{"kind":"task-notification"},"#,
                    r#""message":{"content":"NOR THIS"}}"#
                ),
                r#"{"type":"user","message":{"content":"The owner speaking"}}"#,
            ],
        );
        let head = read_head(&path).expect("head");
        assert_eq!(head.title.as_deref(), Some("The owner speaking"));

        // And the same three records are not turns, which is the other half of the same rule.
        let fold = fold_detailed(&path).expect("fold");
        assert_eq!(fold.metrics.user_turns, 1);
    }

    #[test]
    fn a_session_with_no_user_text_yet_is_untitled_rather_than_missing() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let home = temp.path();
        let dir = home.join(".claude").join("projects").join("-Users-k-w");
        let record = r#"{"type":"attachment","cwd":"/Users/k/w"}"#;
        write_lines(&dir.join(format!("{SID}.jsonl")), &[record]);

        let report = ClaudeAdapter::with_home(home)
            .with_keychain(no_keychain)
            .discover_sessions();
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].title, Session::UNTITLED);
    }

    #[test]
    fn cwd_is_taken_from_the_earliest_record_that_carries_one() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = write_lines(
            &temp.path().join("cwd.jsonl"),
            &[
                r#"{"type":"last-prompt","leafUuid":"x"}"#,
                r#"{"type":"mode","mode":"default"}"#,
                concat!(
                    r#"{"type":"attachment","cwd":"/Users/k/first","gitBranch":"master","#,
                    r#""timestamp":"2026-09-12T10:00:00.000Z"}"#
                ),
                r#"{"type":"attachment","cwd":"/Users/k/second","gitBranch":"other"}"#,
                r#"{"type":"user","cwd":"/Users/k/third","message":{"content":"hello"}}"#,
            ],
        );
        let head = read_head(&path).expect("head");
        assert_eq!(head.cwd.as_deref(), Some(Path::new("/Users/k/first")));
        assert_eq!(head.git_branch.as_deref(), Some("master"));
        assert_eq!(head.first_ts_ms, Some(1_789_207_200_000));
    }

    #[test]
    fn discovery_reads_depth_one_uuid_transcripts_and_nothing_else() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let home = temp.path();
        let project = home.join(".claude").join("projects").join("-Users-k-w");
        let record = r#"{"type":"attachment","cwd":"/Users/k/w"}"#;

        write_lines(&project.join(format!("{SID}.jsonl")), &[record]);
        // Everything below is a real neighbour of a real transcript on this Mac, and none of it is
        // a session: the subagent tree named by the session uuid, `memory/`, the engine's index,
        // and a file whose stem is not a uuid.
        write_lines(
            &project
                .join(SID)
                .join("7c1f0e21-1111-4222-8333-444455556666.jsonl"),
            &[record],
        );
        let stem = "0199c4a1-2b3d-7e4f-8a9b-0c1d2e3f4a5b.jsonl";
        write_lines(&project.join("memory").join(stem), &[record]);
        write_lines(&project.join("sessions-index.json"), &["{}"]);
        write_lines(&project.join("scratch.jsonl"), &[record]);

        let adapter = ClaudeAdapter::with_home(home).with_keychain(no_keychain);
        let report = adapter.discover_sessions();
        assert!(report.problem.is_none());
        let sids: Vec<&str> = report.sessions.iter().map(|s| s.key.sid.as_str()).collect();
        assert_eq!(sids, vec![SID]);
    }

    #[test]
    fn a_machine_that_never_ran_claude_code_is_a_stated_absence_not_a_panic() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let adapter = ClaudeAdapter::with_home(temp.path()).with_keychain(no_keychain);
        let report = adapter.discover_sessions();
        assert!(report.sessions.is_empty());
        let problem = report.problem.expect("a stated problem");
        assert_eq!(problem.kind, ErrorKind::RootMissing);
    }

    #[test]
    fn a_session_whose_working_directory_is_gone_states_why_it_cannot_resume() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let home = temp.path();
        let project = home.join(".claude").join("projects").join("-nowhere");
        write_lines(
            &project.join(format!("{SID}.jsonl")),
            &[r#"{"type":"attachment","cwd":"/nowhere/at/all"}"#],
        );
        let adapter = ClaudeAdapter::with_home(home).with_keychain(no_keychain);
        let report = adapter.discover_sessions();
        let session = &report.sessions[0];
        assert!(!session.resumable);
        assert_eq!(
            session.resume_blocked_reason,
            Some(ResumeBlockedReason::WorkingDirectoryMissing)
        );
    }

    // --------------------------------------------------------------------------------------- //
    // the frozen fold
    // --------------------------------------------------------------------------------------- //

    /// **The single most important test in this file.** `output_tokens` is a streaming counter, so
    /// three records of one `message.id` are one call whose value is the MAX — 812 — and never the
    /// sum (1,057) or the first occurrence (5).
    #[test]
    fn three_streamed_records_of_one_id_fold_to_one_call_with_the_max() {
        let fold = fold_detailed(&fixture("streamed-session.jsonl")).expect("fold");
        let m = fold.metrics;

        assert_eq!(m.api_calls, 2, "two distinct message ids");
        assert_eq!(m.output_tokens, 862, "812 (the max of 5/240/812) + 50");
        assert_ne!(
            m.output_tokens,
            5 + 240 + 812 + 50,
            "summing the records double-counts"
        );
        assert_ne!(m.output_tokens, 5 + 50, "the first occurrence under-counts");
        assert_eq!(m.input_tokens, 130, "120 restated three times, once, + 10");
        assert_eq!(m.cache_read, 41_000);
        assert_eq!(m.cache_write, 300);
        assert_eq!(
            m.user_turns, 2,
            "the tool_result carrier between them is not a turn"
        );
        assert_eq!(m.duration_ms, Some(9_500));
        assert_eq!(
            m.provider_cost_usd, None,
            "a subscription login has no per-call price to state"
        );
        assert_eq!(m.reasoning_tokens, None);
    }

    #[test]
    fn a_tool_use_block_restated_across_streamed_records_is_one_tool_call() {
        let fold = fold_detailed(&fixture("streamed-session.jsonl")).expect("fold");
        // `toolu_1` appears on two records of `msg_A`; `toolu_2` once on `msg_B`.
        assert_eq!(fold.metrics.tool_calls, 2);
    }

    /// The shape Studio's `bd8ed9bb`/`a6b5dd98` fixed: a forked subagent's transcript opens by
    /// restating the parent's dispatching assistant record byte for byte. Dedupe-by-id counts it
    /// once — the rule that already governs streaming is the rule that governs this.
    #[test]
    fn a_restated_fork_head_call_is_counted_once() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let dispatch = concat!(
            r#"{"type":"assistant","timestamp":"2026-09-12T10:00:00.000Z","message":{"id":"msg_F","#,
            r#""content":[{"type":"tool_use","id":"toolu_9","name":"Task","input":{}}],"#,
            r#""usage":{"input_tokens":7,"output_tokens":120,"cache_read_input_tokens":900,"#,
            r#""cache_creation_input_tokens":0}}}"#
        );
        let path = write_lines(
            &temp.path().join("fork.jsonl"),
            &[
                concat!(
                    r#"{"type":"fork-context-ref","agentId":"a1","parentSessionId":"p","#,
                    r#""parentLastUuid":"u","contextLength":799}"#
                ),
                dispatch,
                dispatch,
                r#"{"type":"user","message":{"content":"go"}}"#,
            ],
        );
        let fold = fold_detailed(&path).expect("fold");
        assert_eq!(fold.metrics.api_calls, 1);
        assert_eq!(fold.metrics.output_tokens, 120);
        assert_eq!(fold.metrics.tool_calls, 1);
        // `fork-context-ref` is a type this adapter knows and deliberately counts nothing from.
        assert!(fold.diagnostics.is_empty(), "a known type is not drift");
    }

    /// `thinking_tokens` rides inside the same streaming `usage` payload as `output_tokens`, so it
    /// obeys the same frozen rule: MAX within one `message.id`, summed across ids.
    #[test]
    fn thinking_tokens_fold_by_the_same_max_rule_as_every_other_usage_key() {
        let m = fold_usage(&fixture("thinking-session.jsonl")).expect("fold");

        assert_eq!(m.api_calls, 3, "msg_T (streamed three times), msg_U, msg_V");
        assert_eq!(
            m.reasoning_tokens,
            Some(1_240),
            "1200 (the max of 10/300/1200) + 40"
        );
        assert_ne!(
            m.reasoning_tokens,
            Some(10 + 300 + 1200 + 40),
            "summing the records double-counts"
        );
        assert_ne!(
            m.reasoning_tokens,
            Some(10 + 40),
            "the first occurrence under-counts"
        );
        // A subset of `output_tokens`, never an addend to them: msg_T still reports its own max.
        assert_eq!(m.output_tokens, 1_530 + 60 + 11);
        assert!(m.reasoning_tokens.expect("stated") < m.output_tokens);
    }

    #[test]
    fn a_session_that_states_no_thinking_tokens_reports_none_rather_than_zero() {
        // The engine wrote no `output_tokens_details` at all on any record of this one.
        let stated_nowhere = fold_usage(&fixture("streamed-session.jsonl")).expect("fold");
        assert_eq!(stated_nowhere.reasoning_tokens, None);

        let temp = tempfile::TempDir::new().expect("temp dir");
        // An explicit null is the same statement as an absent key: unstated.
        let nulled = write_lines(
            &temp.path().join("nulled.jsonl"),
            &[concat!(
                r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":1,"#,
                r#""output_tokens":2,"output_tokens_details":null}}}"#
            )],
        );
        assert_eq!(fold_usage(&nulled).expect("fold").reasoning_tokens, None);

        // A stated zero IS a measurement — this call really did no thinking — and is kept.
        let zeroed = write_lines(
            &temp.path().join("zeroed.jsonl"),
            &[concat!(
                r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":1,"#,
                r#""output_tokens":2,"output_tokens_details":{"thinking_tokens":0}}}}"#
            )],
        );
        assert_eq!(fold_usage(&zeroed).expect("fold").reasoning_tokens, Some(0));
    }

    #[test]
    fn a_drifted_thinking_field_is_tallied_without_costing_the_session_its_six_counters() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = write_lines(
            &temp.path().join("drifted-thinking.jsonl"),
            &[concat!(
                r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":1,"#,
                r#""output_tokens":2,"output_tokens_details":{"thinking_tokens":"lots"}}}}"#
            )],
        );
        let fold = fold_detailed(&path).expect("the four frozen counters are unaffected");
        assert_eq!(fold.metrics.output_tokens, 2);
        assert_eq!(
            fold.metrics.reasoning_tokens, None,
            "a figure we cannot read is not a figure"
        );
        assert_eq!(
            fold.diagnostics
                .unknown_types
                .get("(thinking_tokens is not a number)"),
            Some(&1)
        );
    }

    /// The head bound is unchanged — 400 lines or 1 MiB — and `ai-title` is simply one more thing
    /// the early exit waits for. It lands as late as line 198 in the real transcripts here, so a
    /// session whose engine-written title falls outside the bound has no name, and says so.
    #[test]
    fn an_ai_title_inside_the_head_bound_names_the_session_and_one_past_it_does_not() {
        const LATE_SID: &str = "9c8d7e6f-5a4b-4c3d-8e2f-1a0b9c8d7e6f";
        let temp = tempfile::TempDir::new().expect("temp dir");
        let project = temp
            .path()
            .join(".claude")
            .join("projects")
            .join("-Users-k-w");

        let titled = |filler: usize| {
            let mut lines = vec![
                r#"{"type":"attachment","cwd":"/Users/k/w","gitBranch":"master"}"#.to_string(),
                r#"{"type":"user","message":{"content":"the owner's first prompt"}}"#.to_string(),
            ];
            let mode = r#"{"type":"mode","mode":"default"}"#.to_string();
            lines.extend(std::iter::repeat_n(mode, filler));
            let ai = r#"{"type":"ai-title","aiTitle":"Kubernetes  SAS service manager"}"#;
            lines.push(ai.to_string());
            lines
        };
        // Line 253 — inside the bound, and past where cwd and title were both already known.
        let inside = titled(250);
        let outside = titled(HEAD_MAX_LINES + 20);
        fn as_refs(lines: &[String]) -> Vec<&str> {
            lines.iter().map(String::as_str).collect()
        }
        write_lines(&project.join(format!("{SID}.jsonl")), &as_refs(&inside));
        write_lines(
            &project.join(format!("{LATE_SID}.jsonl")),
            &as_refs(&outside),
        );

        let adapter = ClaudeAdapter::with_home(temp.path()).with_keychain(no_keychain);
        let sessions = adapter.discover_sessions().sessions;
        let named = sessions
            .iter()
            .find(|s| s.key.sid == SID)
            .expect("the named session");
        let unnamed = sessions
            .iter()
            .find(|s| s.key.sid == LATE_SID)
            .expect("the unnamed session");

        assert_eq!(
            named.name.as_deref(),
            Some("Kubernetes SAS service manager")
        );
        assert_eq!(
            unnamed.name, None,
            "an ai-title past the bound is not reached, and is not guessed"
        );
        // The engine's title names the row; it never replaces what the owner actually said.
        assert_eq!(named.title, "the owner's first prompt");
        assert_eq!(unnamed.title, "the owner's first prompt");
    }

    /// **The bound is on what is READ, not on what is noticed afterwards.** Measured on this Mac
    /// 2026-09-13: four transcripts hold a single record larger than `HEAD_MAX_BYTES` and the
    /// largest such record is 1,308,288 bytes — so this is the state of real data, not a
    /// hypothetical. A bare `read_until` pulls the whole record into memory and only then compares
    /// the running total, which would read a 14 MB first record whole.
    #[test]
    fn a_record_larger_than_the_head_bound_stops_the_reader_at_the_bound() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = temp.path().join("huge-first-record.jsonl");
        // One valid 1.5 MB record carrying what the head pass wants, and an ordinary record after
        // it. Neither may be reached: the first is cut at the ceiling and the pass is over.
        let giant = format!(
            r#"{{"type":"user","cwd":"/w","message":{{"content":"{}"}}}}"#,
            "x".repeat(1_500_000)
        );
        assert!(
            giant.len() as u64 > HEAD_MAX_BYTES,
            "the record must exceed the bound"
        );
        let after = r#"{"type":"ai-title","aiTitle":"named past the giant"}"#;
        std::fs::write(&path, format!("{giant}\n{after}\n")).expect("fixture written");
        let len = std::fs::metadata(&path).expect("metadata").len();

        let mut reader = BufReader::new(File::open(&path).expect("the transcript opens"));
        let head = read_head_from(&mut reader);
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).expect("what the pass left");
        let taken = len - rest.len() as u64;

        assert!(
            taken <= HEAD_MAX_BYTES,
            "the head pass took {taken} bytes of a {len}-byte transcript; the bound is \
             {HEAD_MAX_BYTES}"
        );
        assert!(
            head.cwd.is_none(),
            "a record cut at the bound is not parsed, so nothing is read out of it"
        );
        assert!(head.name.is_none(), "nothing past the bound is reached");
        assert!(
            head.diagnostics.is_empty(),
            "a record cut by our own ceiling is not the engine drifting: {:?}",
            head.diagnostics.unknown_types
        );
    }

    /// A corrupt record in the middle of a transcript and a half-written one at its end are not
    /// the same event, and only the second is ordinary. The `continue` that tolerates a live
    /// writer's tail applied to every line, so a hole at line 2 left no trace at all.
    #[test]
    fn a_corrupt_record_mid_file_is_stated_and_a_half_written_last_one_is_not() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let corrupt = temp.path().join("corrupt-middle.jsonl");
        std::fs::write(
            &corrupt,
            concat!(
                r#"{"type":"attachment","cwd":"/w"}"#,
                "\n",
                r#"{"type":"user","mess"#,
                "\n",
                r#"{"type":"user","message":{"content":"the real first prompt"}}"#,
                "\n",
            ),
        )
        .expect("fixture written");
        let head = read_head(&corrupt).expect("a hole does not cost the session its row");
        assert_eq!(
            head.title.as_deref(),
            Some("the real first prompt"),
            "the rest of the file is still read"
        );
        assert_eq!(
            head.diagnostics.unknown_types.get("(unparsable record)"),
            Some(&1),
            "a record with more file after it is corruption, and is stated"
        );

        // The very same broken bytes as the LAST line are the live writer, and are silent.
        let live = temp.path().join("live-tail.jsonl");
        std::fs::write(
            &live,
            concat!(
                r#"{"type":"attachment","cwd":"/w"}"#,
                "\n",
                r#"{"type":"user","message":{"content":"the real first prompt"}}"#,
                "\n",
                r#"{"type":"user","mess"#,
            ),
        )
        .expect("fixture written");
        let head = read_head(&live).expect("a live tail is an ordinary state");
        assert_eq!(head.title.as_deref(), Some("the real first prompt"));
        assert!(
            head.diagnostics.is_empty(),
            "the engine appending while we read is not drift: {:?}",
            head.diagnostics.unknown_types
        );
    }

    #[test]
    fn an_unrecognised_record_type_is_tallied_and_the_rest_of_the_file_is_still_read() {
        let fold = fold_detailed(&fixture("streamed-session.jsonl")).expect("fold");
        assert_eq!(
            fold.diagnostics.unknown_types.get("scintilla-quark"),
            Some(&1)
        );
        assert_eq!(
            fold.metrics.api_calls, 2,
            "the unknown type cost nothing else"
        );
    }

    #[test]
    fn a_usage_object_whose_keys_are_all_unrecognised_fails_loud() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = write_lines(
            &temp.path().join("drifted.jsonl"),
            &[r#"{"type":"assistant","message":{"id":"m","usage":{"prompt_tokens":10}}}"#],
        );
        let error =
            fold_detailed(&path).expect_err("a renamed usage schema must not render a number");
        assert_eq!(error.kind, ErrorKind::UnknownShape);
        assert_eq!(
            error.detail,
            ErrorDetail::Fields {
                fields: USAGE_KEYS.iter().map(|k| (*k).to_string()).collect()
            }
        );
    }

    #[test]
    fn an_assistant_record_with_no_message_id_fails_loud_because_the_dedupe_key_is_gone() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = write_lines(
            &temp.path().join("idless.jsonl"),
            &[r#"{"type":"assistant","message":{"usage":{"input_tokens":10,"output_tokens":20}}}"#],
        );
        let error = fold_detailed(&path).expect_err("no id, no counting rule");
        assert_eq!(error.kind, ErrorKind::UnknownShape);
        assert_eq!(
            error.detail,
            ErrorDetail::Fields {
                fields: vec!["message.id".to_string()]
            }
        );
    }

    #[test]
    fn a_missing_usage_key_reads_as_zero_and_a_call_with_no_usage_is_not_an_api_call() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = write_lines(
            &temp.path().join("sparse.jsonl"),
            &[
                // A call that read no cache legitimately states no cache keys.
                concat!(
                    r#"{"type":"assistant","message":{"id":"m1","#,
                    r#""usage":{"input_tokens":4,"output_tokens":9}}}"#
                ),
                // A terminal record with no usage payload at all: a message, not an API call.
                r#"{"type":"assistant","message":{"id":"m2","content":[]}}"#,
            ],
        );
        let m = fold_usage(&path).expect("fold");
        assert_eq!(
            (
                m.api_calls,
                m.input_tokens,
                m.output_tokens,
                m.cache_read,
                m.cache_write
            ),
            (1, 4, 9, 0, 0)
        );
    }

    #[test]
    fn a_half_written_last_line_is_stated_rather_than_fatal() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = temp.path().join("live.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"assistant","message":{"id":"m1","#,
                r#""usage":{"input_tokens":1,"output_tokens":2}}}"#,
                "\n",
                r#"{"type":"assist"#
            ),
        )
        .expect("fixture written");
        let fold = fold_detailed(&path).expect("a live session still counts");
        assert_eq!(fold.metrics.api_calls, 1);
        assert_eq!(
            fold.diagnostics
                .unknown_types
                .get("(half-written last line)"),
            Some(&1)
        );
        assert!(
            !fold
                .diagnostics
                .unknown_types
                .contains_key("(unparsable record)"),
            "the last line of a live session is not a corrupt record"
        );
    }

    /// The same broken bytes one line earlier are a hole in the record rather than a live writer's
    /// tail, and the two are tallied apart. **No counter moves either way** — this is what the
    /// fold could not read, never what it counted — so the frozen rules are untouched.
    #[test]
    fn an_unparsable_record_mid_fold_is_tallied_apart_from_a_live_tail() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let path = temp.path().join("hole.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":1}}}"#,
                "\n",
                r#"{"type":"assist"#,
                "\n",
                r#"{"type":"assistant","message":{"id":"m2","usage":{"output_tokens":2}}}"#,
                "\n",
            ),
        )
        .expect("fixture written");
        let fold = fold_detailed(&path).expect("a hole does not cost the session its numbers");
        assert_eq!(fold.metrics.api_calls, 2, "both whole records still count");
        assert_eq!(fold.metrics.input_tokens, 1);
        assert_eq!(fold.metrics.output_tokens, 2);
        assert_eq!(
            fold.diagnostics.unknown_types.get("(unparsable record)"),
            Some(&1)
        );
        assert!(
            !fold
                .diagnostics
                .unknown_types
                .contains_key("(half-written last line)"),
            "a record with more file after it was not the writer's tail"
        );
    }

    #[test]
    fn a_zero_denominator_kpi_is_undefined_rather_than_zero() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        // A real session that has not yet made an API call: two user turns and nothing else.
        let path = write_lines(
            &temp.path().join("empty.jsonl"),
            &[
                r#"{"type":"user","message":{"content":"first"}}"#,
                r#"{"type":"user","message":{"content":"second"}}"#,
            ],
        );
        let metrics = fold_usage(&path).expect("fold");
        assert_eq!(metrics.user_turns, 2);
        assert_eq!(metrics.api_calls, 0);

        let kpis = metrics.kpis();
        assert_eq!(
            kpis.context_per_call, None,
            "cache_read / 0 calls is undefined"
        );
        assert_eq!(
            kpis.rewrite_ratio, None,
            "cache_write / 0 reads is undefined"
        );
        assert_eq!(
            kpis.batching_ratio, None,
            "tool_calls / 0 calls is undefined"
        );

        // And the same numbers through the state the session actually carries.
        let state = MetricState::Ready {
            metrics,
            basis: MetricBasis::Fold,
            counted_at_ms: 0,
        };
        assert_eq!(state.metrics().expect("ready").kpis().batching_ratio, None);
    }

    // --------------------------------------------------------------------------------------- //
    // timestamps
    // --------------------------------------------------------------------------------------- //

    #[test]
    fn iso_8601_parses_the_engines_spelling_and_refuses_what_it_cannot_read() {
        // The engine's own: 24 characters, milliseconds, `Z`.
        assert_eq!(
            iso8601_to_ms("2026-09-12T10:00:00.000Z"),
            Some(1_789_207_200_000)
        );
        assert_eq!(
            iso8601_to_ms("2026-09-12T10:00:09.500Z"),
            Some(1_789_207_209_500)
        );
        // Studio's stored spelling, and an offset that is not zero.
        assert_eq!(
            iso8601_to_ms("2026-09-12T10:00:00+00:00"),
            Some(1_789_207_200_000)
        );
        assert_eq!(
            iso8601_to_ms("2026-09-12T05:00:00-05:00"),
            Some(1_789_207_200_000)
        );
        assert_eq!(
            iso8601_to_ms("2026-09-12T10:00:00.123456789Z"),
            Some(1_789_207_200_123)
        );
        // The epoch itself, and a leap day, which is where a hand-rolled civil calendar breaks.
        assert_eq!(iso8601_to_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            iso8601_to_ms("2024-02-29T00:00:00Z"),
            Some(1_709_164_800_000)
        );
        assert_eq!(iso8601_to_ms("1969-12-31T23:59:59.000Z"), Some(-1_000));
        // Refused rather than guessed.
        assert_eq!(iso8601_to_ms(""), None);
        assert_eq!(iso8601_to_ms("yesterday"), None);
        assert_eq!(iso8601_to_ms("2026-13-12T10:00:00Z"), None);
        assert_eq!(iso8601_to_ms("2026-09-12 10:00:00 UTC"), None);
    }

    #[test]
    fn a_duration_needs_both_ends_and_is_never_negative() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let turn = r#"{"type":"user","message":{"content":"x"}}"#;
        let unstamped = write_lines(&temp.path().join("nots.jsonl"), &[turn]);
        assert_eq!(fold_usage(&unstamped).expect("fold").duration_ms, None);

        let backwards = write_lines(
            &temp.path().join("back.jsonl"),
            &[
                r#"{"type":"user","timestamp":"2026-09-12T10:00:05.000Z","message":{"content":"x"}}"#,
                r#"{"type":"user","timestamp":"2026-09-12T10:00:00.000Z","message":{"content":"y"}}"#,
            ],
        );
        let span = fold_usage(&backwards).expect("fold").duration_ms;
        assert_eq!(span, None, "a clock that ran backwards is not a span");
    }

    // --------------------------------------------------------------------------------------- //
    // identity and capacity
    // --------------------------------------------------------------------------------------- //

    #[test]
    fn identity_reads_the_six_safe_fields_and_a_bounded_tail_of_the_account_id() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        std::fs::write(
            temp.path().join(".claude.json"),
            r#"{"projects":{"/Users/k/w":{"history":["a","b"]}},"oauthAccount":{
                "accountUuid":"316b0000-0000-0000-0000-0000000abcdef","emailAddress":"k@example.com",
                "displayName":"Khalid","organizationName":"Integrated Analytic Solutions",
                "organizationType":"claude_team","seatTier":"team_tier_1",
                "userRateLimitTier":"default_claude_max_5x"}}"#,
        )
        .expect("config written");

        let identity = ClaudeAdapter::with_home(temp.path())
            .with_keychain(no_keychain)
            .read_identity();
        assert!(identity.signed_in);
        assert_eq!(identity.label.as_deref(), Some("k@example.com"));
        assert_eq!(
            identity.organization.as_deref(),
            Some("Integrated Analytic Solutions")
        );
        assert_eq!(identity.plan.as_deref(), Some("default_claude_max_5x"));
        assert_eq!(identity.tier.as_deref(), Some("team_tier_1"));
        assert_eq!(identity.account_short.as_deref(), Some("abcdef"));
        assert!(identity.problem.is_none());
    }

    #[test]
    fn a_config_with_no_oauth_account_is_signed_out_and_not_a_problem() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        std::fs::write(temp.path().join(".claude.json"), r#"{"projects":{}}"#)
            .expect("config written");
        let identity = ClaudeAdapter::with_home(temp.path())
            .with_keychain(no_keychain)
            .read_identity();
        assert!(!identity.signed_in);
        assert!(
            identity.problem.is_none(),
            "signed out is an ordinary state"
        );
    }

    #[test]
    fn a_null_bucket_renders_an_absent_window_never_zero_percent() {
        let raw = std::fs::read(fixture("usage-endpoint-body.json")).expect("fixture");
        let body: Value = serde_json::from_slice(&raw).expect("fixture parses");
        let capacity = capacity_from_body(&body, Some("max".to_string()), 1_000);

        assert!(capacity.supported);
        assert!(capacity.problem.is_none());
        assert_eq!(
            capacity.windows.len(),
            1,
            "the null five-hour bucket is absent, not 0%"
        );
        let weekly = capacity.windows[0];
        assert_eq!(weekly.name, CapacityWindowName::Weekly);
        assert_eq!(weekly.window_minutes, 10_080);
        assert_eq!(weekly.used_pct, 37.5);
        assert_eq!(weekly.resets_at_ms, Some(1_789_794_000_000));
        assert!(!capacity
            .windows
            .iter()
            .any(|w| w.name == CapacityWindowName::FiveHour));
        assert_eq!(capacity.plan.as_deref(), Some("max"));
    }

    #[test]
    fn an_unrecognised_body_shape_states_an_unknown_shape_and_draws_no_window() {
        // Neither window key: the endpoint was renamed under us.
        let text = r#"{"rate_limits":{"five_hour":{"utilization":10}}}"#;
        let renamed: Value = serde_json::from_str(text).expect("json");
        let capacity = capacity_from_body(&renamed, None, 1_000);
        assert!(capacity.windows.is_empty());
        let problem = capacity.problem.expect("a stated problem");
        assert_eq!(problem.kind, ErrorKind::UnknownShape);
        assert_eq!(
            problem.detail,
            ErrorDetail::Fields {
                fields: vec!["five_hour".to_string(), "seven_day".to_string()]
            }
        );

        // A window key that is present but is no longer an object, and one whose utilization
        // stopped being a number: both would otherwise render a plausible figure.
        let text = r#"{"five_hour":"high","seven_day":{"utilization":true}}"#;
        let drifted: Value = serde_json::from_str(text).expect("json");
        let capacity = capacity_from_body(&drifted, None, 1_000);
        assert!(capacity.windows.is_empty());
        let problem = capacity.problem.expect("a stated problem");
        assert_eq!(problem.kind, ErrorKind::UnknownShape);
        assert_eq!(
            problem.detail,
            ErrorDetail::Fields {
                fields: vec!["five_hour".to_string(), "seven_day.utilization".to_string()]
            }
        );

        // And a body that is not an object at all.
        assert_eq!(
            capacity_from_body(&Value::Array(vec![]), None, 1_000)
                .problem
                .expect("problem")
                .kind,
            ErrorKind::UnknownShape
        );
    }

    #[test]
    fn a_body_whose_every_bucket_is_null_is_supported_with_nothing_to_draw() {
        let body: Value =
            serde_json::from_str(r#"{"five_hour":null,"seven_day":null}"#).expect("json");
        let capacity = capacity_from_body(&body, None, 1_000);
        assert!(
            capacity.supported,
            "the endpoint answered; it simply had nothing to report"
        );
        assert!(capacity.windows.is_empty());
        assert!(capacity.problem.is_none());
    }

    #[test]
    fn an_absent_credential_and_an_api_key_login_are_two_different_stated_absences() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let absent = ClaudeAdapter::with_home(temp.path()).with_keychain(no_keychain);
        let error = absent.load_login().err().expect("no credential to load");
        assert_eq!(error.kind, ErrorKind::NoCredential);

        std::fs::create_dir_all(temp.path().join(".claude")).expect("dir");
        std::fs::write(
            temp.path().join(".claude").join(".credentials.json"),
            r#"{"apiKeyHelper":"x"}"#,
        )
        .expect("creds written");
        let api_key = ClaudeAdapter::with_home(temp.path()).with_keychain(no_keychain);
        let error = api_key
            .load_login()
            .err()
            .expect("an api-key login has no windows");
        assert_eq!(
            error.kind,
            ErrorKind::Unsupported,
            "the 5-hour and weekly windows are a subscription concept"
        );
    }

    #[test]
    fn http_statuses_classify_into_a_refused_login_or_an_answering_endpoint() {
        assert_eq!(http_status_error(401).kind, ErrorKind::CredentialRefused);
        assert_eq!(http_status_error(403).kind, ErrorKind::CredentialRefused);
        assert_eq!(http_status_error(429).kind, ErrorKind::HttpStatus);
        assert_eq!(
            http_status_error(429).detail,
            ErrorDetail::Status { code: 429 }
        );
        assert_eq!(
            http_status_error(503).detail,
            ErrorDetail::Status { code: 503 }
        );
    }

    /// The leak table for this adapter: a real token in the real place, through every value the
    /// file hands back.
    #[test]
    fn a_stored_token_reaches_no_capacity_no_identity_and_no_error() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        std::fs::create_dir_all(temp.path().join(".claude")).expect("dir");
        std::fs::write(
            temp.path().join(".claude").join(".credentials.json"),
            format!(
                r#"{{"claudeAiOauth":{{"accessToken":"{SENTINEL}","refreshToken":"{SENTINEL}-refresh",
                "subscriptionType":"max","expiresAt":1789552800000}}}}"#
            ),
        )
        .expect("creds written");
        std::fs::write(
            temp.path().join(".claude.json"),
            format!(
                r#"{{"oauthAccount":{{"emailAddress":"k@example.com","accountUuid":"316b-abcdef"}},
                "cachedToken":"{SENTINEL}"}}"#
            ),
        )
        .expect("config written");

        let adapter = ClaudeAdapter::with_home(temp.path()).with_keychain(no_keychain);
        let login = adapter.load_login().expect("the token loads");
        assert_eq!(login.plan.as_deref(), Some("max"));
        // It is really there — compared, never exposed, because `Secret::expose` appears exactly
        // once in this file and that one use builds the Authorization header.
        assert_eq!(login.token, Secret::new(SENTINEL));
        assert!(!format!("{:?}", login.token).contains("LEAKCANARY"));

        let identity = adapter.read_identity();
        assert!(
            !format!("{identity:?}").contains("LEAKCANARY"),
            "identity Debug leaked the token"
        );
        assert_eq!(identity.label.as_deref(), Some("k@example.com"));

        let text = r#"{"five_hour":{"utilization":12.5},"seven_day":null}"#;
        let body: Value = serde_json::from_str(text).expect("json");
        let capacity = capacity_from_body(&body, login.plan.clone(), 1_000);
        assert!(
            !format!("{capacity:?}").contains("LEAKCANARY"),
            "capacity Debug leaked the token"
        );

        // Every error this file can build, over both renderings.
        let errors = vec![
            http_status_error(401),
            http_status_error(429),
            EngineError::of(PROVIDER, ErrorKind::NoCredential),
            EngineError::of(PROVIDER, ErrorKind::Transport),
            EngineError::unknown_shape(PROVIDER, &["five_hour", "seven_day"]),
            capacity_from_body(&Value::Bool(true), None, 1_000)
                .problem
                .expect("problem"),
            EngineError::of(PROVIDER, ErrorKind::Unsupported),
        ];
        for error in errors {
            let json = serde_json::to_string(&error).expect("serializes");
            assert!(
                !json.contains("LEAKCANARY"),
                "{:?} leaked through serde",
                error.kind
            );
            let debug = format!("{error:?}");
            assert!(
                !debug.contains("LEAKCANARY"),
                "{:?} leaked through Debug",
                error.kind
            );
        }
    }

    // --------------------------------------------------------------------------------------- //
    // real data
    // --------------------------------------------------------------------------------------- //

    /// Discovery against the owner's own `~/.claude/projects`, read-only.
    ///
    /// Ran on this Mac on 2026-09-13: 18 project directories, 51 depth-1 transcripts, every stem a
    /// uuid. Returns early rather than failing when the root is absent, so a fresh machine — or
    /// CI — still passes.
    #[test]
    fn real_claude_sessions_on_this_machine_all_carry_uuid_ids() {
        let adapter = ClaudeAdapter::new();
        if !adapter.projects_dir().is_dir() {
            eprintln!("no ~/.claude/projects on this machine — real-data check skipped");
            return;
        }
        let report = adapter.discover_sessions();
        assert!(
            report.problem.is_none(),
            "a present root must not report a problem"
        );
        assert!(
            !report.sessions.is_empty(),
            "a present root has at least one session"
        );
        let named = report.sessions.iter().filter(|s| s.name.is_some()).count();
        let found = report.sessions.len();
        eprintln!("{found} sessions discovered, {named} of them named by an ai-title");
        for session in &report.sessions {
            let sid = &session.key.sid;
            assert!(is_uuid(sid), "discovery returned a non-uuid sid: {sid}");
            assert!(session.key.is_valid());
            assert!(
                session.last_active_ms > 0,
                "a transcript always has an mtime"
            );
            // Never the decoded directory name: the cwd is the engine's own, so it is absolute.
            if let Some(cwd) = &session.cwd {
                assert!(
                    cwd.is_absolute(),
                    "a recorded cwd is absolute: {}",
                    cwd.display()
                );
            }
        }
    }

    /// Folds the three largest real transcripts and prints the six counters, for cross-checking
    /// against Demo Studio's own parse of the same files. `cargo test -- --nocapture` to read
    /// it. Ran on this Mac, 2026-09-13.
    #[test]
    fn folding_the_largest_real_sessions_reports_its_counters() {
        let adapter = ClaudeAdapter::new();
        if !adapter.projects_dir().is_dir() {
            eprintln!("no ~/.claude/projects on this machine — real-data fold skipped");
            return;
        }
        let mut sessions = adapter.discover_sessions().sessions;
        sessions.sort_by_key(|s| match &s.source_signature {
            SourceSignature::Claude { main, .. } => std::cmp::Reverse(main.size),
            _ => std::cmp::Reverse(0),
        });
        for session in sessions.iter().take(3) {
            let path = &session.source.paths[0];
            let fold = fold_detailed(path).expect("a real transcript folds");
            let m = fold.metrics;
            // A char-wise prefix, never `&sid[..8]`: byte-slicing a `String` panics on a boundary
            // that is not a character one, and the only thing standing between that and real
            // engine data is `is_uuid` gating discovery three functions away. A label in an
            // `eprintln!` is not worth a panic that depends on a guard somewhere else.
            let short: String = session.key.sid.chars().take(8).collect();
            eprintln!(
                "{} calls={} tools={} turns={} in={} out={} read={} write={} think={:?} \
                 dur={:?} unknown={:?}",
                short,
                m.api_calls,
                m.tool_calls,
                m.user_turns,
                m.input_tokens,
                m.output_tokens,
                m.cache_read,
                m.cache_write,
                m.reasoning_tokens,
                m.duration_ms,
                fold.diagnostics.unknown_types,
            );
            assert!(m.api_calls > 0, "a multi-megabyte transcript has API calls");
        }
    }
}
