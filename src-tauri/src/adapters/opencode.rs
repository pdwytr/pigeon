//! OpenCode's SQLite session store, read live and read-only.
//!
//! OpenCode keeps everything in one database — `~/.local/share/opencode/opencode.db`, 59 MB with
//! a 3.8 MB WAL beside it on this Mac, 2026-09-13 — and the engine writes to it while Pigeon
//! reads. Three consequences, each a line of code below rather than a wish:
//!
//! 1. **Two spellings, chosen by one `stat`.** While OpenCode is running, `mode=ro` is the only
//!    safe form: `immutable=1` promises SQLite the file cannot change, so it skips the WAL and
//!    reads the last checkpointed image — here that is 3.8 MB of the owner's most recent work
//!    missing, and a torn view of a store being appended to right now. The moment the owner
//!    quits OpenCode the two swap roles, because SQLite deletes `-wal` and `-shm` on the last
//!    close and `mode=ro` then has to rebuild the shared-memory index — by *writing* two files
//!    into the engine's own directory, or by refusing the open outright. With no `-wal` beside
//!    the database there is nothing for `immutable=1` to skip, so that is the spelling used, and
//!    it touches nothing. Neither can create the database, so one that is not there stays not
//!    there (invariant: read-only on sources) and surfaces as `RootMissing` naming the path.
//! 2. **Metrics come off the `session` row.** OpenCode already totals tokens and its own dollar
//!    cost per session, so counting is a column read, not a fold — [`MetricBasis::Columns`]. The
//!    dollar figure is the *engine's own*; Pigeon never invents a price.
//! 3. **Counts are one grouped query, not one per session.** 24 root sessions over 1,536 message
//!    rows and 5,785 part rows on this machine; N+1 would be 48 extra statements fired at a live
//!    writer for numbers two `GROUP BY`s already have.
//!
//! Measured on this Mac, 2026-09-13, against the live database:
//!
//! - `session`: 29 rows — 24 with `parent_id IS NULL AND time_archived IS NULL`, 5 children
//!   (subagents), 0 archived.
//! - `time_updated` is epoch **milliseconds**: `datetime(1789332473422/1000,'unixepoch')` is
//!   `2026-09-13 20:47:53`, while `datetime(1789332473422,'unixepoch')` is out of range and
//!   returns nothing at all. Unlike the other two engines, no scaling is applied here.
//! - `message` has **no `role` column** — the role lives in its `data` JSON (1,275 assistant,
//!   261 user). `part` likewise types itself in `data` (1,279 `tool` of 5,785).
//! - `account` has 0 rows but 8 columns, two of which are `access_token` and `refresh_token`.
//!   Only `email` is ever named in a statement here.
//! - A **cleanly-closed** store — the state quitting OpenCode leaves behind — has neither `-shm`
//!   nor `-wal`. Opening that `mode=ro` through rusqlite's bundled SQLite 3.46.0 succeeds *by
//!   creating* `opencode.db-shm` and an empty `opencode.db-wal` beside the owner's database and
//!   leaving them there; Apple's `sqlite3` 3.43.2 refuses the identical open with
//!   `SQLITE_CANTOPEN(14)`, and so does 3.46.0 when the directory is not writable — which is how
//!   an idle engine's 24 sessions can read as "the database is missing". `immutable=1` reads the
//!   same file correctly and creates nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension};

use super::wait::{OwnerWait, WaitPolicy, WaitSignal};
use super::{ProviderAdapter, ProviderSessionReport, SessionCandidate};
use crate::api::errors::{EngineError, ErrorDetail, ErrorKind, Secret};
use crate::domain::{
    Capacity, Identity, LiveState, MetricBasis, Metrics, ProviderId, ProviderSummary,
    ResumeBlockedReason, Session, SessionKey, SourceSignature, SourceSummary,
};
use crate::util::{now_ms, tidy_title};

const PROVIDER: ProviderId = ProviderId::OpenCode;
const DB_FILE: &str = "opencode.db";
const AUTH_FILE: &str = "auth.json";
const GO_USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
const TITLE_MAX: usize = 120;

/// Short enough that a checkpointing writer never stalls a refresh, long enough to ride out one.
/// A busy that outlives it is retried exactly once by [`OpenCodeAdapter::with_db`].
const BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const PENDING_APPROVAL_GRACE_MS: i64 = 500;

/// Read in the SELECT; `parent_id` and `time_archived` are read in the WHERE. All thirteen are
/// verified present before a statement runs, so a renamed column fails loud instead of silently
/// dropping a number.
const SESSION_REQUIRED: [&str; 13] = [
    "id",
    "parent_id",
    "directory",
    "title",
    "time_created",
    "time_updated",
    "time_archived",
    "cost",
    "tokens_input",
    "tokens_output",
    "tokens_reasoning",
    "tokens_cache_read",
    "tokens_cache_write",
];
const MESSAGE_REQUIRED: [&str; 2] = ["session_id", "data"];
const PART_REQUIRED: [&str; 2] = ["session_id", "data"];

const SESSION_FIELDS: &str = "id, directory, title, time_created, time_updated, cost, \
     tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write";

/// One entry per root session: its counters, or the reason they could not be produced.
pub type SessionMetrics = BTreeMap<String, Result<Metrics, EngineError>>;

/// The activity marker OpenCode persisted for one session.
///
/// This is intentionally a small adapter contract: the status service does not know OpenCode's
/// JSON vocabulary, and the UI does not need to know either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCodeActivity {
    pub state: LiveState,
    pub since_ms: Option<i64>,
    pub raw_word: Option<String>,
    pub evidence: Vec<String>,
}

/// The newest part of a session, projected out of its JSON so the policy and the turn reading do
/// not each re-parse it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OpenCodePart {
    since_ms: i64,
    kind: Option<String>,
    status: Option<String>,
    reason: Option<String>,
    assistant_completed: bool,
    interrupted: bool,
    /// The part's status is `running`/`pending` and its tool is `question`: the engine is blocked
    /// on an answer. Measured 2026-09-16 — a `question` cannot be running without being asked.
    question: bool,
}

impl OpenCodePart {
    fn project(
        since_ms: i64,
        parsed: &serde_json::Value,
        message: Option<&serde_json::Value>,
    ) -> Self {
        let kind = parsed.get("type").and_then(serde_json::Value::as_str);
        let status = parsed
            .get("state")
            .and_then(|value| value.get("status"))
            .and_then(serde_json::Value::as_str);
        let reason = parsed.get("reason").and_then(serde_json::Value::as_str);
        let tool = parsed.get("tool").and_then(serde_json::Value::as_str);
        let assistant_completed = message
            .and_then(|message| message.get("time")?.get("completed")?.as_i64())
            .is_some();
        // **An interrupt leaves the step unfinished.** OpenCode records `MessageAbortedError` on
        // the assistant message and closes it, but the newest part is still a `step-start` or a
        // `tool` whose state never left `running`. Reading the part alone therefore reports
        // `Running` for a turn the owner has already stopped.
        let interrupted = message
            .and_then(|message| message.get("error")?.get("name")?.as_str())
            .is_some_and(|name| name == "MessageAbortedError");
        // The question case is deliberately the *last* of the three to hold: an aborted or
        // completed message means the turn is over, and a `question` part left behind must not
        // reopen it. This preserves the order the single classifier used before the policy split.
        let question = !interrupted
            && !assistant_completed
            && kind == Some("tool")
            && matches!(status, Some("pending" | "running"))
            && tool == Some("question");
        Self {
            since_ms,
            kind: kind.map(str::to_string),
            status: status.map(str::to_string),
            reason: reason.map(str::to_string),
            assistant_completed,
            interrupted,
            question,
        }
    }

    /// The running/waiting/unknown reading once the three owner cases are set aside.
    fn turn_reading(&self) -> (LiveState, String, String) {
        let kind = self.kind.as_deref();
        let status = self.status.as_deref();
        let reason = self.reason.as_deref();
        if self.assistant_completed {
            // **A completed message means no turn is in flight**, and that outranks the shape of the
            // last part. Measured on this machine 2026-09-16: of every session's newest part, the
            // only one whose message was NOT completed was the one genuinely `tool|running`.
            return (
                LiveState::Waiting,
                kind.unwrap_or("completed").into(),
                "latest assistant message is completed".into(),
            );
        }
        match (kind, status, reason) {
            (Some("step-finish"), _, Some("stop")) => (
                LiveState::Waiting,
                "step-finish:stop".into(),
                "latest OpenCode step finished and stopped".into(),
            ),
            (Some("step-finish"), _, Some(reason)) => (
                LiveState::Running,
                format!("step-finish:{reason}"),
                format!("latest OpenCode step finished with {reason}; turn continues"),
            ),
            (Some("step-finish"), _, None) => (
                LiveState::Running,
                "step-finish".into(),
                "latest OpenCode step has no stop reason; turn may continue".into(),
            ),
            // The policy has already handled a `question` tool; what is left pending here is an
            // unconfirmed approval, which is honestly unknown rather than a guess.
            (Some("permission"), _, _) | (Some("tool"), Some("pending"), _) => (
                LiveState::Unknown,
                kind.unwrap_or("pending").into(),
                "latest OpenCode tool is pending, but approval has not been confirmed".into(),
            ),
            (_, Some("running" | "pending"), _) | (Some("step-start"), _, _) => (
                LiveState::Running,
                kind.unwrap_or("running").into(),
                "latest OpenCode part is unfinished".into(),
            ),
            (Some("tool"), Some("error"), _) => (
                LiveState::Waiting,
                "tool:error".into(),
                "latest OpenCode tool ended with an error or was rejected".into(),
            ),
            (Some(kind), _, _)
                if matches!(
                    kind,
                    "text" | "reasoning" | "tool" | "file" | "patch" | "subtask" | "step-start"
                ) =>
            {
                (
                    LiveState::Running,
                    kind.into(),
                    "latest OpenCode part is part of an unfinished turn".into(),
                )
            }
            _ => (
                LiveState::Unknown,
                kind.unwrap_or("unknown").into(),
                "latest OpenCode part has no recognized activity marker".into(),
            ),
        }
    }
}

/// The facts OpenCode's [`WaitPolicy`] decides over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct OpenCodeWaitFacts {
    /// Evidence that an approval is outstanding, from the event stream or the permission table.
    permission: Option<String>,
    question: bool,
    interrupted: bool,
}

/// OpenCode's [`WaitPolicy`]: a pending permission for permission, a `question` tool for question,
/// and `MessageAbortedError` for interruption.
///
/// **What it cannot see, stated plainly.** OpenCode does not persist a live approval ask in the
/// `permission` table — measured on this machine 2026-09-18, an `external_directory` ask held the
/// turn for 8.6 minutes with zero rows. The pending-part event is the only on-disk trace, and it
/// fires only for the tool shapes OpenCode emits one for; a wait it does not record is `None`
/// here rather than a guess.
struct OpenCodeWait<'a> {
    facts: &'a OpenCodeWaitFacts,
}

impl<'a> OpenCodeWait<'a> {
    fn new(facts: &'a OpenCodeWaitFacts) -> Self {
        Self { facts }
    }
}

impl WaitPolicy for OpenCodeWait<'_> {
    fn permission(&self) -> Option<WaitSignal> {
        self.facts.permission.as_ref().map(|reason| {
            WaitSignal::new(OwnerWait::Permission)
                .word("permission")
                .because(reason.clone())
        })
    }

    fn question(&self) -> Option<WaitSignal> {
        self.facts.question.then(|| {
            WaitSignal::new(OwnerWait::Question)
                .word("question")
                .because("OpenCode is waiting on an answer to a question".to_string())
        })
    }

    fn interruption(&self) -> Option<WaitSignal> {
        self.facts.interrupted.then(|| {
            WaitSignal::new(OwnerWait::Interruption)
                .word("MessageAbortedError")
                .because(
                    "the latest OpenCode turn was aborted, so the engine is back at its prompt"
                        .to_string(),
                )
        })
    }
}

/// The adapter. Rooted at a directory so a test can point it at a fixture without faking `$HOME`.
pub struct OpenCodeAdapter {
    root: PathBuf,
}

impl OpenCodeAdapter {
    /// OpenCode's counters are already summed per session, so no fold is involved.
    pub const BASIS: MetricBasis = MetricBasis::Columns;

    pub fn new() -> Self {
        Self {
            root: default_root(),
        }
    }

    /// Point the adapter at any directory holding `opencode.db` and `auth.json`.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn database_path(&self) -> PathBuf {
        self.root.join(DB_FILE)
    }

    pub fn auth_path(&self) -> PathBuf {
        self.root.join(AUTH_FILE)
    }

    /// Read the newest OpenCode turn marker for a session.
    ///
    /// `step-start` and unfinished tool parts mean the engine is still working. A `step-finish`
    /// or completed assistant message means it is sitting idle. OpenCode represents an approval
    /// pause as a pending tool/permission part, which is intentionally reported as unknown until
    /// Pigeon has a reliable approval-state signal.
    pub fn read_activity(&self, sid: &str) -> Result<OpenCodeActivity, EngineError> {
        let sid = sid.trim();
        if sid.is_empty() {
            return Err(EngineError::of(PROVIDER, ErrorKind::Path));
        }
        self.with_db(|db| db.activity(sid))
    }

    /// Every root session's counters in three statements total, whatever the session count.
    ///
    /// Each entry holds exactly what the per-session [`Self::read_metrics`] would have returned
    /// for that id, so the cheap path cannot go quiet where the loud one speaks: a session whose
    /// `message` or `part` rows hold something that is not JSON comes back as the same
    /// `UnknownShape` error, which the metrics service stores as `MetricState::Unavailable` — a
    /// stated absence on that row. It used to `continue` past those, leaving the caller unable to
    /// tell "this session has no metrics" from "we could not read it".
    ///
    /// Returning the skipped ids in a second `Vec` is the smaller diff by a line or two, and was
    /// rejected: the caller would then have to invent the error it renders, which is how two
    /// paths start disagreeing about the same session. A `Result` per id carries the reason with
    /// the id and needs no new type.
    pub fn read_all_metrics(&self) -> Result<SessionMetrics, EngineError> {
        self.with_db(|db| {
            let rows = db.sessions(None)?;
            let counts = db.counts(None)?;
            let mut out = SessionMetrics::new();
            for row in rows {
                let tally = counts.get(&row.id).copied().unwrap_or_default();
                let counted = if tally.unparsed > 0 {
                    Err(fields_error(&["message.data", "part.data"]))
                } else {
                    Ok(metrics_for(&row, &tally))
                };
                out.insert(row.id, counted);
            }
            Ok(out)
        })
    }

    /// One session's counters, for the metrics service's per-session path.
    pub fn read_metrics(&self, sid: &str) -> Result<Metrics, EngineError> {
        let sid = sid.trim();
        if sid.is_empty() {
            return Err(EngineError::of(PROVIDER, ErrorKind::Path));
        }
        self.with_db(|db| {
            let row = db
                .sessions(Some(sid))?
                .into_iter()
                .next()
                // The row was there at discovery and is not there now: archived or deleted
                // between the two reads. A stated absence, never a zeroed Metrics.
                .ok_or_else(|| EngineError::root_missing(PROVIDER, &db.path))?;
            let tally = db.counts(Some(sid))?.remove(sid).unwrap_or_default();
            if tally.unparsed > 0 {
                return Err(fields_error(&["message.data", "part.data"]));
            }
            Ok(metrics_for(&row, &tally))
        })
    }

    /// Open, run, and on a busy database open and run once more. The retry is the whole job, not
    /// just the open: `busy_timeout` covers the lock wait, this covers the checkpoint behind it.
    ///
    /// The spelling is re-decided on the retry rather than reused, because the checkpoint we were
    /// waiting behind is exactly the event that can delete the WAL.
    fn with_db<T>(&self, job: impl Fn(&Db) -> Result<T, EngineError>) -> Result<T, EngineError> {
        let path = self.database_path();
        let run = || Db::open(&path, Spelling::for_database(&path)).and_then(|db| job(&db));
        match run() {
            Err(ref e) if e.kind == ErrorKind::Busy => run(),
            other => other,
        }
    }

    /// The signed-in email, when the `account` table exists and holds a row.
    ///
    /// `email` is the only column ever named: the table also holds `access_token` and
    /// `refresh_token`, and `SELECT *` here would pull both into this process for no reason.
    fn read_account_email(&self) -> Result<Option<String>, EngineError> {
        self.with_db(|db| {
            let have = db.columns("account")?;
            if have.is_empty() {
                // No such table. An absence — this OpenCode signs in through auth.json alone.
                return Ok(None);
            }
            if !have.contains("email") {
                return Err(fields_error(&["account.email"]));
            }
            let mut stmt = db.prepare("SELECT email FROM account LIMIT 1")?;
            let mut rows = stmt.query([]).map_err(|e| db.classify(&e))?;
            let first = rows.next().map_err(|e| db.classify(&e))?;
            let Some(row) = first else { return Ok(None) };
            let email: Option<String> = row.get(0).map_err(|e| db.classify(&e))?;
            Ok(email.filter(|e| !e.trim().is_empty()))
        })
    }
}

impl Default for OpenCodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderAdapter for OpenCodeAdapter {
    fn provider(&self) -> ProviderId {
        PROVIDER
    }

    fn discover_sessions(&self) -> ProviderSessionReport {
        let source = SourceSummary::single(self.database_path());
        match self.with_db(|db| db.sessions(None)) {
            Ok(rows) => ProviderSessionReport {
                sessions: rows.iter().map(|row| row.candidate(&source)).collect(),
                problem: None,
            },
            Err(error) => ProviderSessionReport::problem(error),
        }
    }

    fn read_identity(&self) -> Identity {
        let read_at_ms = now_ms();
        let providers = match self.read_auth_providers() {
            Ok(providers) => providers,
            Err(problem) => return Identity::absent(PROVIDER, read_at_ms, Some(problem)),
        };
        // The database is a nicety here, not the identity: auth.json already answers "is anyone
        // signed in". A database that is simply absent therefore adds no problem; one that is
        // busy or misshapen does, because that is drift the owner should see.
        let (label, problem) = match self.read_account_email() {
            Ok(label) => (label, None),
            Err(e) if e.kind == ErrorKind::RootMissing => (None, None),
            Err(e) => (None, Some(e)),
        };
        Identity {
            provider: PROVIDER,
            signed_in: !providers.is_empty(),
            label,
            organization: None,
            plan: None,
            tier: None,
            mode: None,
            account_short: None,
            providers: Some(providers),
            read_at_ms,
            problem,
        }
    }

    fn read_capacity(&self) -> Capacity {
        let read_at_ms = now_ms();
        let key = match self.read_auth_key("opencode-go") {
            Ok(key) => key,
            Err(error) => return Capacity::problem(PROVIDER, read_at_ms, error),
        };
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return Capacity::problem(
                PROVIDER,
                read_at_ms,
                EngineError::of(PROVIDER, ErrorKind::Io),
            );
        };
        runtime.block_on(self.fetch_go_usage(key, read_at_ms))
    }
}

impl OpenCodeAdapter {
    async fn fetch_go_usage(&self, key: Secret, read_at_ms: i64) -> Capacity {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(client) => client,
            Err(_) => {
                return Capacity::problem(
                    PROVIDER,
                    read_at_ms,
                    EngineError::of(PROVIDER, ErrorKind::Io),
                )
            }
        };
        let request = key
            .with_value(|token| reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")));
        let Ok(mut authorization) = request else {
            return Capacity::problem(
                PROVIDER,
                read_at_ms,
                EngineError::unknown_shape(PROVIDER, &["auth.json.opencode-go.key"]),
            );
        };
        authorization.set_sensitive(true);
        let response = match client
            .get(GO_USAGE_URL)
            .header(reqwest::header::AUTHORIZATION, authorization)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => {
                return Capacity::problem(
                    PROVIDER,
                    read_at_ms,
                    EngineError::of(PROVIDER, ErrorKind::Transport),
                )
            }
        };
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Capacity::problem(
                PROVIDER,
                read_at_ms,
                EngineError::new(
                    Some(PROVIDER),
                    if status == 401 || status == 403 {
                        ErrorKind::NoCredential
                    } else {
                        ErrorKind::HttpStatus
                    },
                    ErrorDetail::Status { code: status },
                ),
            );
        }
        let Ok(body) = response.text().await else {
            return Capacity::problem(
                PROVIDER,
                read_at_ms,
                EngineError::of(PROVIDER, ErrorKind::Transport),
            );
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
            return Capacity::problem(
                PROVIDER,
                read_at_ms,
                EngineError::unknown_shape(PROVIDER, &["usage"]),
            );
        };
        match go_capacity(&value, read_at_ms) {
            Ok(windows) => Capacity {
                provider: PROVIDER,
                supported: true,
                windows,
                plan: Some("OpenCode Go".into()),
                stale: false,
                source_age_s: None,
                reached_limit: None,
                read_at_ms,
                problem: None,
            },
            Err(fields) => Capacity::problem(
                PROVIDER,
                read_at_ms,
                EngineError::unknown_shape(PROVIDER, &fields),
            ),
        }
    }

    /// auth.json is `{ provider: { type, key } }` — on this Mac, `opencode` and `opencode-go`,
    /// both `"type": "api"`. Only the name and the type are read; `key` is a credential and is
    /// never copied out of the parse.
    fn read_auth_providers(&self) -> Result<Vec<ProviderSummary>, EngineError> {
        let path = self.auth_path();
        let text = std::fs::read_to_string(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => EngineError::with(
                PROVIDER,
                ErrorKind::NoCredential,
                ErrorDetail::Path {
                    path: path.display().to_string(),
                },
            ),
            _ => EngineError::from_io(PROVIDER, &e, &path),
        })?;
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| fields_error(&["auth.json: object of provider → { type }"]))?;
        let serde_json::Value::Object(map) = parsed else {
            return Err(fields_error(&["auth.json: object of provider → { type }"]));
        };
        Ok(map
            .iter()
            .map(|(name, value)| ProviderSummary {
                name: name.clone(),
                // A provider entry with no `type` is recorded as unknown rather than guessed into
                // "api" or "oauth" — the two differ in what the owner is billed for.
                kind: value
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
            })
            .collect())
    }

    fn read_auth_key(&self, provider: &str) -> Result<Secret, EngineError> {
        let path = self.auth_path();
        let text = std::fs::read_to_string(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => EngineError::with(
                PROVIDER,
                ErrorKind::NoCredential,
                ErrorDetail::Path {
                    path: path.display().to_string(),
                },
            ),
            _ => EngineError::from_io(PROVIDER, &e, &path),
        })?;
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| EngineError::unknown_shape(PROVIDER, &["auth.json"]))?;
        let key = parsed
            .get(provider)
            .and_then(|entry| entry.get("key"))
            .and_then(|key| key.as_str())
            .filter(|key| !key.is_empty());
        key.map(|key| Secret::new(key.to_string()))
            .ok_or_else(|| EngineError::of(PROVIDER, ErrorKind::NoCredential))
    }
}

fn go_capacity(
    value: &serde_json::Value,
    _read_at_ms: i64,
) -> Result<Vec<crate::domain::CapacityWindow>, Vec<&'static str>> {
    let usage = value
        .get("usage")
        .and_then(|usage| usage.as_object())
        .ok_or_else(|| vec!["usage"])?;
    let windows = [
        ("rolling", crate::domain::CapacityWindowName::FiveHour),
        ("weekly", crate::domain::CapacityWindowName::Weekly),
        ("monthly", crate::domain::CapacityWindowName::Monthly),
    ];
    let mut out = Vec::with_capacity(windows.len());
    for (key, name) in windows {
        let item = usage
            .get(key)
            .and_then(|item| item.as_object())
            .ok_or_else(|| vec!["usage"])?;
        let percent = item
            .get("percent")
            .and_then(|percent| percent.as_f64())
            .filter(|percent| (0.0..=100.0).contains(percent))
            .ok_or_else(|| vec!["usage", "percent"])?;
        let resets_at_ms = match item.get("resetsAt") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(timestamp)) => Some(
                crate::adapters::claude::iso8601_to_ms(timestamp)
                    .ok_or_else(|| vec!["usage", "resetsAt"])?,
            ),
            Some(_) => return Err(vec!["usage", "resetsAt"]),
        };
        out.push(crate::domain::CapacityWindow {
            name,
            window_minutes: name.minutes(),
            used_pct: percent,
            resets_at_ms,
        });
    }
    Ok(out)
}

/// One `session` row, exactly the columns this file names.
#[derive(Clone, Debug)]
struct SessionRow {
    id: String,
    directory: String,
    title: String,
    time_created: i64,
    time_updated: i64,
    cost: Option<f64>,
    tokens_input: i64,
    tokens_output: i64,
    tokens_reasoning: i64,
    tokens_cache_read: i64,
    tokens_cache_write: i64,
}

impl SessionRow {
    fn candidate(&self, source: &SourceSummary) -> SessionCandidate {
        let cwd = if self.directory.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(&self.directory))
        };
        let title = tidy_title(&self.title, TITLE_MAX);
        let title = if title.is_empty() {
            Session::UNTITLED.to_string()
        } else {
            title
        };
        // Whether the CLI is installed and whether the folder still exists are the session
        // service's to decide; "the row itself names nowhere to launch" is the adapter's.
        let blocked = cwd
            .is_none()
            .then_some(ResumeBlockedReason::MissingWorkingDirectory);
        SessionCandidate {
            key: SessionKey::new(PROVIDER, self.id.clone()),
            cwd,
            title,
            // `session.slug` exists but is a url token, not a name the owner chose.
            name: None,
            // OpenCode records no branch anywhere on the session row.
            git_branch: None,
            first_active_ms: Some(self.time_created),
            last_active_ms: self.time_updated,
            // `time_archived` is the only close signal and archived rows never reach here.
            closed_at_ms: None,
            resumable: blocked.is_none(),
            resume_blocked_reason: blocked,
            source: source.clone(),
            diagnostics: Default::default(),
            source_signature: SourceSignature::OpenCode {
                time_updated_ms: self.time_updated,
            },
        }
    }
}

/// Per-session tallies from the two child tables.
#[derive(Clone, Copy, Debug, Default)]
struct Counts {
    api_calls: u64,
    user_turns: u64,
    tool_calls: u64,
    /// Rows whose `data` is not JSON at all. Non-zero means drift, and is reported, not skipped.
    unparsed: u64,
}

fn metrics_for(row: &SessionRow, tally: &Counts) -> Metrics {
    let span = row.time_updated - row.time_created;
    Metrics {
        input_tokens: row.tokens_input.max(0) as u64,
        output_tokens: row.tokens_output.max(0) as u64,
        cache_read: row.tokens_cache_read.max(0) as u64,
        cache_write: row.tokens_cache_write.max(0) as u64,
        api_calls: tally.api_calls,
        tool_calls: tally.tool_calls,
        user_turns: tally.user_turns,
        // A row whose update predates its creation has no duration, only a bad clock.
        duration_ms: (span >= 0).then_some(span as u64),
        reasoning_tokens: Some(row.tokens_reasoning.max(0) as u64),
        // The engine's own dollar figure, passed through untouched. Pigeon computes no price.
        provider_cost_usd: row.cost,
    }
}

/// An open read-only connection plus the path it came from, so every error can name it.
struct Db {
    conn: Connection,
    path: PathBuf,
}

impl Db {
    fn open(path: &Path, spelling: Spelling) -> Result<Self, EngineError> {
        let uri = to_uri(path, spelling);
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        let conn = Connection::open_with_flags(&uri, flags).map_err(|e| classify(&e, path))?;
        // Belt to the flag's braces: `query_only` refuses a write even if some future code path
        // asks for one, and costs nothing.
        conn.execute_batch("PRAGMA query_only = 1;")
            .map_err(|e| classify(&e, path))?;
        conn.busy_timeout(BUSY_TIMEOUT)
            .map_err(|e| classify(&e, path))?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    fn classify(&self, err: &rusqlite::Error) -> EngineError {
        classify(err, &self.path)
    }

    fn prepare(&self, sql: &str) -> Result<rusqlite::Statement<'_>, EngineError> {
        self.conn.prepare(sql).map_err(|e| self.classify(&e))
    }

    /// The column names a table actually has. An empty set means no such table.
    fn columns(&self, table: &str) -> Result<BTreeSet<String>, EngineError> {
        // `table` is one of four literals in this file and never reaches here from outside it,
        // so the interpolation cannot become an injection; PRAGMA takes no bound parameter.
        let mut stmt = self.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([]).map_err(|e| self.classify(&e))?;
        let mut out = BTreeSet::new();
        while let Some(row) = rows.next().map_err(|e| self.classify(&e))? {
            let name: String = row.get(1).map_err(|e| self.classify(&e))?;
            out.insert(name);
        }
        Ok(out)
    }

    /// Fail loud before a statement runs: name every column this file expects and does not find.
    fn require(&self, table: &str, wanted: &[&str]) -> Result<(), EngineError> {
        let have = self.columns(table)?;
        let missing: Vec<String> = wanted
            .iter()
            .filter(|c| !have.contains(**c))
            .map(|c| format!("{table}.{c}"))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        Err(EngineError::with(
            PROVIDER,
            ErrorKind::UnknownShape,
            ErrorDetail::Fields { fields: missing },
        ))
    }

    /// Root sessions, or one session by id.
    ///
    /// `parent_id IS NOT NULL` is a subagent — it belongs to its parent's row, not to the list.
    /// `time_archived IS NOT NULL` is a session the owner put away.
    fn sessions(&self, sid: Option<&str>) -> Result<Vec<SessionRow>, EngineError> {
        self.require("session", &SESSION_REQUIRED)?;
        let sql = match sid {
            Some(_) => format!("SELECT {SESSION_FIELDS} FROM session WHERE id = ?1"),
            None => format!(
                "SELECT {SESSION_FIELDS} FROM session \
                 WHERE parent_id IS NULL AND time_archived IS NULL"
            ),
        };
        let mut stmt = self.prepare(&sql)?;
        let mut rows = match sid {
            Some(sid) => stmt.query([sid]).map_err(|e| self.classify(&e))?,
            None => stmt.query([]).map_err(|e| self.classify(&e))?,
        };
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(|e| self.classify(&e))? {
            let id: String = row.get(0).map_err(|e| self.classify(&e))?;
            // An id is the only handle a session has — it is the cache key and the argv the
            // resume is built from. A blank one cannot be either, so it is not a session.
            if id.trim().is_empty() {
                continue;
            }
            out.push(SessionRow {
                id,
                directory: row.get(1).map_err(|e| self.classify(&e))?,
                title: row.get(2).map_err(|e| self.classify(&e))?,
                time_created: row.get(3).map_err(|e| self.classify(&e))?,
                time_updated: row.get(4).map_err(|e| self.classify(&e))?,
                cost: row.get(5).map_err(|e| self.classify(&e))?,
                tokens_input: row.get(6).map_err(|e| self.classify(&e))?,
                tokens_output: row.get(7).map_err(|e| self.classify(&e))?,
                tokens_reasoning: row.get(8).map_err(|e| self.classify(&e))?,
                tokens_cache_read: row.get(9).map_err(|e| self.classify(&e))?,
                tokens_cache_write: row.get(10).map_err(|e| self.classify(&e))?,
            });
        }
        Ok(out)
    }

    /// Both child tables tallied per session in two grouped statements — never one per session.
    ///
    /// `message` carries no `role` column on this machine (verified 2026-09-13): the role is in
    /// its `data` JSON, so the role test is a `json_extract`, guarded by `json_valid` because
    /// `json_extract` on a non-JSON row aborts the whole statement and would take 23 innocent
    /// sessions' counts down with it. The guarded rows are counted, not ignored.
    fn counts(&self, sid: Option<&str>) -> Result<BTreeMap<String, Counts>, EngineError> {
        self.require("message", &MESSAGE_REQUIRED)?;
        self.require("part", &PART_REQUIRED)?;
        let mut out: BTreeMap<String, Counts> = BTreeMap::new();

        let where_one = if sid.is_some() {
            "WHERE session_id = ?1 "
        } else {
            ""
        };
        let messages = format!(
            "SELECT session_id, \
             SUM(CASE WHEN json_valid(data) AND json_extract(data, '$.role') = 'assistant' \
                      THEN 1 ELSE 0 END), \
             SUM(CASE WHEN json_valid(data) AND json_extract(data, '$.role') = 'user' \
                      THEN 1 ELSE 0 END), \
             SUM(CASE WHEN json_valid(data) THEN 0 ELSE 1 END) \
             FROM message {where_one}GROUP BY session_id"
        );
        self.tally(&messages, sid, &mut out, |slot, a, b, unparsed| {
            slot.api_calls = a;
            slot.user_turns = b;
            slot.unparsed += unparsed;
        })?;

        let parts = format!(
            "SELECT session_id, \
             SUM(CASE WHEN json_valid(data) AND json_extract(data, '$.type') = 'tool' \
                      THEN 1 ELSE 0 END), \
             0, \
             SUM(CASE WHEN json_valid(data) THEN 0 ELSE 1 END) \
             FROM part {where_one}GROUP BY session_id"
        );
        self.tally(&parts, sid, &mut out, |slot, a, _b, unparsed| {
            slot.tool_calls = a;
            slot.unparsed += unparsed;
        })?;
        Ok(out)
    }

    fn activity(&self, sid: &str) -> Result<OpenCodeActivity, EngineError> {
        self.require("part", &["session_id", "time_updated", "data"])?;
        self.require("message", &["session_id", "time_updated", "data"])?;

        // Gather the facts for the shared policy before deciding anything. Permission is the one
        // case with two independent sources; either is enough.
        //
        // The mutable part row can move from pending to running while the approval UI is still
        // visible. The append-only event stream preserves the transition Pigeon needs to render
        // NeedsYou, so consult its newest part update as well as the permission table.
        let permission = if self.pending_part_event(sid)? {
            Some("OpenCode's event stream records a pending owner approval".to_string())
        } else if self.pending_permission(sid)? {
            // The permission table is the authoritative human-waiting signal. A pending tool part
            // alone is ambiguous: it can be an ordinary tool still executing. The permission row is
            // scoped through the session's project, so it promotes only this session.
            Some("OpenCode has a pending permission for this session".to_string())
        } else {
            None
        };

        let part = self.newest_part(sid)?;
        let facts = OpenCodeWaitFacts {
            permission,
            question: part.as_ref().is_some_and(|p| p.question),
            interrupted: part.as_ref().is_some_and(|p| p.interrupted),
        };
        let wait = OpenCodeWait::new(&facts);

        // The shared policy decides the three owner cases in one place; only the turn shape below
        // is OpenCode's own.
        if let Some(signal) = wait.owner_wait() {
            let since_ms = if signal.case == OwnerWait::Permission {
                None
            } else {
                part.as_ref().map(|p| p.since_ms)
            };
            return Ok(OpenCodeActivity {
                state: signal.case.state(),
                since_ms,
                raw_word: Some(
                    signal
                        .raw_word
                        .unwrap_or_else(|| signal.case.word().to_string()),
                ),
                evidence: signal.evidence.into_iter().collect(),
            });
        }

        let Some(part) = part else {
            return Ok(OpenCodeActivity {
                state: LiveState::Unknown,
                since_ms: None,
                raw_word: None,
                evidence: vec!["no OpenCode turn marker exists for this session".into()],
            });
        };
        let (state, raw_word, explanation) = part.turn_reading();
        Ok(OpenCodeActivity {
            state,
            since_ms: Some(part.since_ms),
            raw_word: Some(raw_word),
            evidence: vec![explanation],
        })
    }

    /// The newest part for a session, projected into the facts the policy and the turn reading
    /// need. `None` when the session has no part rows at all.
    fn newest_part(&self, sid: &str) -> Result<Option<OpenCodePart>, EngineError> {
        let mut stmt = self.prepare(
            "SELECT p.time_updated, p.data, m.data
             FROM part p LEFT JOIN message m ON m.id = p.message_id
             WHERE p.session_id = ?1
             ORDER BY p.time_updated DESC, p.id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([sid]).map_err(|e| self.classify(&e))?;
        let Some(row) = rows.next().map_err(|e| self.classify(&e))? else {
            return Ok(None);
        };
        let since_ms: i64 = row.get(0).map_err(|e| self.classify(&e))?;
        let data: String = row.get(1).map_err(|e| self.classify(&e))?;
        let message_data: Option<String> = row.get(2).map_err(|e| self.classify(&e))?;
        let parsed: serde_json::Value = serde_json::from_str(&data)
            .map_err(|_| fields_error(&["part.data: valid JSON object"]))?;
        let message = message_data
            .as_deref()
            .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok());
        Ok(Some(OpenCodePart::project(
            since_ms,
            &parsed,
            message.as_ref(),
        )))
    }

    fn pending_permission(&self, sid: &str) -> Result<bool, EngineError> {
        let columns = self.columns("permission")?;
        if columns.is_empty() || !columns.iter().any(|column| column == "project_id") {
            return Ok(false);
        }
        let mut stmt = self.prepare(
            "SELECT 1 FROM permission p
             JOIN session s ON s.project_id = p.project_id
             WHERE s.id = ?1 LIMIT 1",
        )?;
        stmt.query_row([sid], |_row| Ok(()))
            .map(|_| true)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(self.classify(&other)),
            })
    }

    fn pending_part_event(&self, sid: &str) -> Result<bool, EngineError> {
        let columns = self.columns("event")?;
        if columns.is_empty() {
            return Ok(false);
        }
        let mut stmt = self.prepare(
            "SELECT data FROM event
             WHERE aggregate_id = ?1 AND type = 'message.part.updated.1'
             ORDER BY seq DESC LIMIT 1",
        )?;
        let data: Option<String> = stmt
            .query_row([sid], |row| row.get(0))
            .optional()
            .map_err(|error| self.classify(&error))?;
        let Some(data) = data else { return Ok(false) };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) else {
            return Ok(false);
        };
        let pending = value
            .get("part")
            .and_then(|part| part.get("type"))
            .and_then(serde_json::Value::as_str)
            == Some("tool")
            && value
                .get("part")
                .and_then(|part| part.get("state"))
                .and_then(|state| state.get("status"))
                .and_then(serde_json::Value::as_str)
                == Some("pending");
        let Some(observed_at) = value.get("time").and_then(serde_json::Value::as_i64) else {
            return Ok(false);
        };
        Ok(pending && now_ms().saturating_sub(observed_at) >= PENDING_APPROVAL_GRACE_MS)
    }

    fn tally(
        &self,
        sql: &str,
        sid: Option<&str>,
        out: &mut BTreeMap<String, Counts>,
        apply: impl Fn(&mut Counts, u64, u64, u64),
    ) -> Result<(), EngineError> {
        let mut stmt = self.prepare(sql)?;
        let mut rows = match sid {
            Some(sid) => stmt.query([sid]).map_err(|e| self.classify(&e))?,
            None => stmt.query([]).map_err(|e| self.classify(&e))?,
        };
        while let Some(row) = rows.next().map_err(|e| self.classify(&e))? {
            let key: String = row.get(0).map_err(|e| self.classify(&e))?;
            let a: i64 = row.get(1).map_err(|e| self.classify(&e))?;
            let b: i64 = row.get(2).map_err(|e| self.classify(&e))?;
            let unparsed: i64 = row.get(3).map_err(|e| self.classify(&e))?;
            let slot = out.entry(key).or_default();
            apply(
                slot,
                a.max(0) as u64,
                b.max(0) as u64,
                unparsed.max(0) as u64,
            );
        }
        Ok(())
    }
}

/// How the database is named in the `file:` URI. These two spellings are the only ones this
/// adapter may use, and which one is legal depends entirely on the state of the store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Spelling {
    /// `mode=ro` — honours the WAL. Mandatory whenever one exists.
    WalAware,
    /// `immutable=1` — reads the file as it lies and creates nothing. Legal only with no WAL.
    Immutable,
}

impl Spelling {
    /// One `stat` decides it, and the decision is the whole safety argument.
    ///
    /// A `<db>-wal` beside the database means its contents are data — OpenCode is running, or it
    /// crashed before checkpointing — and `immutable=1` would answer from the image *behind* that
    /// WAL: on this Mac, 3.8 MB of the owner's newest work silently absent. So a WAL present is
    /// `mode=ro`, always, even if that open then fails; a wrong number is worse than a stated
    /// failure.
    ///
    /// No `-wal` means the last connection closed cleanly and SQLite deleted both it and the
    /// `-shm` on the way out. There is nothing left for `immutable=1` to skip, so the hazard the
    /// module doc forbids it for cannot arise — while `mode=ro` on that same file has to rebuild
    /// the `-shm` it needs, and measured on this Mac 2026-09-13 that means one of two bad
    /// outcomes: bundled SQLite 3.46.0 writes `opencode.db-shm` and an empty `opencode.db-wal`
    /// into the engine's directory (invariant 1 says we never write there), and where it cannot
    /// write it fails with `SQLITE_CANTOPEN` instead, taking every session off the dashboard.
    ///
    /// The race — OpenCode starting between this `stat` and the read — costs at most the few
    /// milliseconds of writes it manages in that window, and cannot resurrect the 3.8 MB case:
    /// that WAL's existence is precisely what this test excludes.
    fn for_database(db: &Path) -> Self {
        if wal_path(db).exists() {
            Self::WalAware
        } else {
            Self::Immutable
        }
    }

    fn query(self) -> &'static str {
        match self {
            Self::WalAware => "mode=ro",
            Self::Immutable => "immutable=1",
        }
    }
}

/// `opencode.db-wal`, spelled the way SQLite spells it: the suffix is appended to the whole file
/// name, not substituted for the extension, so `Path::with_extension` is the wrong tool.
fn wal_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push("-wal");
    PathBuf::from(name)
}

/// `~/.local/share/opencode` — OpenCode uses the XDG layout on macOS too, not
/// `~/Library/Application Support`, so `dirs::data_dir()` would point at the wrong place.
fn default_root() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"));
    base.join("opencode")
}

/// The `file:` URI form, percent-encoded.
///
/// SQLite reads `?` as the start of the query and `#` as a fragment, and decodes `%XX` in the
/// path — so a folder named `notes?` or `50%` would silently open a different file, or no file.
/// Everything outside the unreserved set is escaped rather than enumerating what happens to be
/// safe; a space becomes `%20`. `:` and `\` are kept for a Windows `C:\…` spelling.
fn to_uri(path: &Path, spelling: Spelling) -> String {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len() + 24);
    out.push_str("file:");
    for byte in raw.as_bytes() {
        let ch = *byte as char;
        let keep =
            ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~' | '/' | ':' | '\\');
        if keep {
            out.push(ch);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out.push('?');
    out.push_str(spelling.query());
    out
}

/// Whether the database is *absent*, as opposed to merely unreachable.
///
/// `Path::exists` answers false for both, and the two deserve different sentences: an absence is
/// "OpenCode has no data here" and sends nobody looking, while a directory we may not `stat`
/// through is an I/O failure the owner can act on. Only `NotFound` is an absence.
fn is_genuinely_absent(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => false,
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

fn fields_error(fields: &[&str]) -> EngineError {
    EngineError::unknown_shape(PROVIDER, fields)
}

/// Classify a rusqlite error and **drop it**.
///
/// `format!("{e}")` on a `rusqlite::Error` can quote the statement text, and a statement can
/// contain a value; the typed error vocabulary exists so that never reaches the View. The only
/// string ever taken from the error is a column name from our own `SELECT` list.
fn classify(err: &rusqlite::Error, path: &Path) -> EngineError {
    let at_path = || ErrorDetail::Path {
        path: path.display().to_string(),
    };
    match err {
        // Asked for a row, got none. An absence, not a failure.
        rusqlite::Error::QueryReturnedNoRows => {
            EngineError::with(PROVIDER, ErrorKind::RootMissing, at_path())
        }
        // A column we named holds a type we cannot read — the schema moved under us.
        rusqlite::Error::InvalidColumnType(_, name, _) => EngineError::with(
            PROVIDER,
            ErrorKind::UnknownShape,
            ErrorDetail::Fields {
                fields: vec![name.clone()],
            },
        ),
        rusqlite::Error::SqliteFailure(e, _) => match e.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => {
                EngineError::with(PROVIDER, ErrorKind::Busy, at_path())
            }
            // `SQLITE_CANTOPEN` is not proof of absence, and saying "OpenCode has no data at
            // <path>" about a 59 MB file that is right there sends the owner hunting for
            // something nobody deleted. Measured on this Mac, 2026-09-13: a `chmod 000` database
            // answers `CannotOpen(14)`, and so does a cleanly-closed WAL database whose `-shm`
            // SQLite is not allowed to create (Apple's `sqlite3` 3.43.2, or a read-only
            // directory). Only a path that genuinely is not there is an absence.
            ErrorCode::CannotOpen if is_genuinely_absent(path) => {
                EngineError::with(PROVIDER, ErrorKind::RootMissing, at_path())
            }
            ErrorCode::CannotOpen => EngineError::with(PROVIDER, ErrorKind::Io, at_path()),
            ErrorCode::NotADatabase => {
                EngineError::with(PROVIDER, ErrorKind::UnknownShape, at_path())
            }
            ErrorCode::TypeMismatch => {
                EngineError::with(PROVIDER, ErrorKind::UnknownShape, at_path())
            }
            _ => EngineError::with(PROVIDER, ErrorKind::Io, at_path()),
        },
        _ => EngineError::with(PROVIDER, ErrorKind::Io, at_path()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::CapacityWindowName;
    use std::path::Path;
    use tempfile::TempDir;

    /// The live schema, copied verbatim from `sqlite_master` on this Mac, 2026-09-13, minus the
    /// foreign keys (whose parent tables are not part of what this adapter reads). Tests run
    /// against the real column set, including the ten columns this file never names, so a
    /// `SELECT *` creeping in would be visible.
    const SESSION_DDL: &str = "CREATE TABLE `session` (
        `id` text PRIMARY KEY, `project_id` text NOT NULL, `parent_id` text, `slug` text NOT NULL,
        `directory` text NOT NULL, `title` text NOT NULL, `version` text NOT NULL,
        `share_url` text, `summary_additions` integer, `summary_deletions` integer,
        `summary_files` integer, `summary_diffs` text, `revert` text, `permission` text,
        `time_created` integer NOT NULL, `time_updated` integer NOT NULL,
        `time_compacting` integer, `time_archived` integer, `workspace_id` text, `path` text,
        `agent` text, `model` text, `cost` real DEFAULT 0 NOT NULL,
        `tokens_input` integer DEFAULT 0 NOT NULL, `tokens_output` integer DEFAULT 0 NOT NULL,
        `tokens_reasoning` integer DEFAULT 0 NOT NULL,
        `tokens_cache_read` integer DEFAULT 0 NOT NULL,
        `tokens_cache_write` integer DEFAULT 0 NOT NULL, `metadata` text)";

    /// The live DDL has `cost real DEFAULT 0 NOT NULL`, so a NULL cost cannot occur there today.
    /// This variant exists to pin the reader's behaviour for a schema where it can — an older
    /// store, or a later one that relaxes the constraint.
    const SESSION_DDL_NULLABLE_COST: &str = "CREATE TABLE `session` (
        `id` text PRIMARY KEY, `parent_id` text, `directory` text NOT NULL, `title` text NOT NULL,
        `time_created` integer NOT NULL, `time_updated` integer NOT NULL, `time_archived` integer,
        `cost` real, `tokens_input` integer DEFAULT 0 NOT NULL,
        `tokens_output` integer DEFAULT 0 NOT NULL, `tokens_reasoning` integer DEFAULT 0 NOT NULL,
        `tokens_cache_read` integer DEFAULT 0 NOT NULL,
        `tokens_cache_write` integer DEFAULT 0 NOT NULL)";

    const MESSAGE_DDL: &str = "CREATE TABLE `message` (
        `id` text PRIMARY KEY, `session_id` text NOT NULL, `time_created` integer NOT NULL,
        `time_updated` integer NOT NULL, `data` text NOT NULL)";

    const PART_DDL: &str = "CREATE TABLE `part` (
        `id` text PRIMARY KEY, `message_id` text NOT NULL, `session_id` text NOT NULL,
        `time_created` integer NOT NULL, `time_updated` integer NOT NULL, `data` text NOT NULL)";

    const ACCOUNT_DDL: &str = "CREATE TABLE `account` (
        `id` text PRIMARY KEY, `email` text NOT NULL, `url` text NOT NULL,
        `access_token` text NOT NULL, `refresh_token` text NOT NULL, `token_expiry` integer,
        `time_created` integer NOT NULL, `time_updated` integer NOT NULL)";

    /// A real epoch-**millisecond** `time_updated` lifted from the live store on 2026-09-13:
    /// `datetime(1789332473422/1000,'unixepoch')` is `2026-09-13 20:47:53`.
    const REAL_TIME_UPDATED_MS: i64 = 1_789_332_473_422;
    const REAL_TIME_CREATED_MS: i64 = 1_789_220_163_572;

    /// A writer connection, kept alive by the caller so every read in a test happens while
    /// another connection holds the database open in WAL mode — the live condition.
    struct Fixture {
        _dir: TempDir,
        root: PathBuf,
        writer: Connection,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_ddl(SESSION_DDL)
        }

        fn with_ddl(session_ddl: &str) -> Self {
            let dir = TempDir::new().expect("temp dir");
            let root = dir.path().to_path_buf();
            let writer = Connection::open(root.join(DB_FILE)).expect("create db");
            let mode: String = writer
                .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
                .expect("set wal");
            assert_eq!(
                mode, "wal",
                "the fixture must reproduce the live WAL journal mode"
            );
            writer.execute_batch(session_ddl).expect("session ddl");
            writer.execute_batch(MESSAGE_DDL).expect("message ddl");
            writer.execute_batch(PART_DDL).expect("part ddl");
            Self {
                _dir: dir,
                root,
                writer,
            }
        }

        fn adapter(&self) -> OpenCodeAdapter {
            OpenCodeAdapter::with_root(&self.root)
        }

        fn insert_session(&self, row: &TestSession) {
            let full = self
                .writer
                .prepare("SELECT 1 FROM pragma_table_info('session') WHERE name = 'project_id'")
                .and_then(|mut s| s.exists([]))
                .expect("probe");
            let sql = if full {
                "INSERT INTO session (id, project_id, parent_id, slug, directory, title, version,
                 time_created, time_updated, time_archived, cost, tokens_input, tokens_output,
                 tokens_reasoning, tokens_cache_read, tokens_cache_write)
                 VALUES (?1, 'prj', ?2, 'slug', ?3, ?4, '1.0', ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                 ?12, ?13)"
            } else {
                "INSERT INTO session (id, parent_id, directory, title, time_created, time_updated,
                 time_archived, cost, tokens_input, tokens_output, tokens_reasoning,
                 tokens_cache_read, tokens_cache_write)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"
            };
            self.writer
                .execute(
                    sql,
                    rusqlite::params![
                        row.id,
                        row.parent_id,
                        row.directory,
                        row.title,
                        row.time_created,
                        row.time_updated,
                        row.time_archived,
                        row.cost,
                        row.tokens_input,
                        row.tokens_output,
                        row.tokens_reasoning,
                        row.tokens_cache_read,
                        row.tokens_cache_write,
                    ],
                )
                .expect("insert session");
        }

        fn insert_message(&self, id: &str, session_id: &str, data: &str) {
            self.writer
                .execute(
                    "INSERT INTO message (id, session_id, time_created, time_updated, data)
                     VALUES (?1, ?2, 1, 2, ?3)",
                    rusqlite::params![id, session_id, data],
                )
                .expect("insert message");
        }

        fn insert_part(&self, id: &str, session_id: &str, data: &str) {
            self.writer
                .execute(
                    "INSERT INTO part (id, message_id, session_id, time_created, time_updated,
                     data) VALUES (?1, 'msg', ?2, 1, 2, ?3)",
                    rusqlite::params![id, session_id, data],
                )
                .expect("insert part");
        }

        fn write_auth(&self, body: &str) {
            std::fs::write(self.root.join(AUTH_FILE), body).expect("write auth.json");
        }

        /// Close the writer and hand back the directory it left behind — the state OpenCode is
        /// in whenever the owner is not running it. SQLite deletes `-wal` and `-shm` on the last
        /// connection's close, and that is exactly the state `mode=ro` alone cannot open.
        fn quit_engine(self) -> ClosedFixture {
            let Fixture { _dir, root, writer } = self;
            writer.close().expect("close the writer cleanly");
            assert!(
                !root.join("opencode.db-wal").exists(),
                "a clean close deletes the WAL; without that this fixture proves nothing"
            );
            assert!(
                !root.join("opencode.db-shm").exists(),
                "and the shm with it"
            );
            ClosedFixture { _dir, root }
        }
    }

    /// A fixture whose writer is gone. Every other fixture here holds its writer open for the
    /// life of the test, which keeps `-shm` alive and hides the engine-not-running case.
    struct ClosedFixture {
        _dir: TempDir,
        root: PathBuf,
    }

    impl ClosedFixture {
        fn adapter(&self) -> OpenCodeAdapter {
            OpenCodeAdapter::with_root(&self.root)
        }
        fn db(&self) -> PathBuf {
            self.root.join(DB_FILE)
        }
        /// Everything the engine's directory holds, sorted — so a test can say "and nothing
        /// else appeared" without naming the files it hopes are missing.
        fn files(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.root)
                .expect("read dir")
                .map(|e| e.expect("entry").file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            names
        }
    }

    /// A session row as named fields, so adding one never grows an argument list.
    #[derive(Clone)]
    struct TestSession {
        id: String,
        parent_id: Option<String>,
        directory: String,
        title: String,
        time_created: i64,
        time_updated: i64,
        time_archived: Option<i64>,
        cost: Option<f64>,
        tokens_input: i64,
        tokens_output: i64,
        tokens_reasoning: i64,
        tokens_cache_read: i64,
        tokens_cache_write: i64,
    }

    impl TestSession {
        fn new(id: &str) -> Self {
            Self {
                id: id.to_string(),
                parent_id: None,
                directory: "/Users/khalid/Documents/Projects/feather".to_string(),
                title: "A session".to_string(),
                time_created: REAL_TIME_CREATED_MS,
                time_updated: REAL_TIME_UPDATED_MS,
                time_archived: None,
                cost: Some(1.5),
                tokens_input: 1_059,
                tokens_output: 155_563,
                tokens_reasoning: 45_592,
                tokens_cache_read: 117_139_812,
                tokens_cache_write: 2_098_894,
            }
        }
        fn child_of(mut self, parent: &str) -> Self {
            self.parent_id = Some(parent.to_string());
            self
        }
        fn archived_at(mut self, ms: i64) -> Self {
            self.time_archived = Some(ms);
            self
        }
        fn cost(mut self, cost: Option<f64>) -> Self {
            self.cost = cost;
            self
        }
    }

    fn sids(report: &ProviderSessionReport) -> Vec<String> {
        report.sessions.iter().map(|s| s.key.sid.clone()).collect()
    }

    #[test]
    fn activity_reads_an_unfinished_step_as_running() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_part("p1", "ses_root", r#"{"type":"step-start"}"#);

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::Running);
        assert_eq!(activity.raw_word.as_deref(), Some("step-start"));
    }

    /// An interrupt leaves the last part looking unfinished, so the part alone reads `Running`.
    ///
    /// Measured on this machine 2026-09-16: OpenCode marks an interrupted turn with
    /// `error.name = "MessageAbortedError"` and closes the message, but the newest part is still a
    /// `step-start` (or a `tool` stuck at `running`). This is the owner's report — "interruptions
    /// show as running" — reproduced.
    #[test]
    fn activity_reads_an_aborted_turn_as_waiting_though_its_step_is_unfinished() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_message(
            "msg",
            "ses_root",
            r#"{"role":"assistant","time":{"created":1,"completed":2},"error":{"name":"MessageAbortedError","data":{"message":"Aborted"}}}"#,
        );
        fx.insert_part("p1", "ses_root", r#"{"type":"step-start"}"#);

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::Waiting);
        assert_eq!(activity.raw_word.as_deref(), Some("MessageAbortedError"));
    }

    /// An OpenCode `question` tool blocks the agent until the owner answers.
    ///
    /// Measured 2026-09-16: the part's status is `running` for the entire wait (the event stream
    /// goes `pending -> running -> completed`), so the status alone says the agent is working while
    /// it is in fact waiting on a person. The tool's name is what distinguishes it.
    #[test]
    fn activity_reads_a_running_question_tool_as_needing_the_owner() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_message(
            "msg",
            "ses_root",
            r#"{"role":"assistant","time":{"created":1}}"#,
        );
        fx.insert_part(
            "p1",
            "ses_root",
            r#"{"type":"tool","tool":"question","state":{"status":"running","input":{"questions":[{"question":"Add the test?"}]}}}"#,
        );

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::NeedsYou);
        assert_eq!(activity.raw_word.as_deref(), Some("question"));
    }

    /// The guard against over-correcting: a tool that is genuinely running belongs to a message
    /// that is NOT completed, and that must still read `Running`.
    #[test]
    fn activity_still_reads_a_running_tool_whose_message_is_not_completed() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_message(
            "msg",
            "ses_root",
            r#"{"role":"assistant","time":{"created":1}}"#,
        );
        fx.insert_part(
            "p1",
            "ses_root",
            r#"{"type":"tool","state":{"status":"running"}}"#,
        );

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::Running);
    }

    #[test]
    fn activity_reads_a_finished_step_as_waiting() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_part(
            "p1",
            "ses_root",
            r#"{"type":"step-finish","reason":"stop"}"#,
        );

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::Waiting);
        assert_eq!(activity.raw_word.as_deref(), Some("step-finish:stop"));
    }

    #[test]
    fn activity_keeps_a_tool_call_chain_running_until_the_stop_marker() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_part(
            "p1",
            "ses_root",
            r#"{"type":"step-finish","reason":"tool-calls"}"#,
        );

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::Running);
        assert_eq!(activity.raw_word.as_deref(), Some("step-finish:tool-calls"));
    }

    #[test]
    fn activity_uses_completed_assistant_message_for_idle_text_parts() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_message(
            "msg",
            "ses_root",
            r#"{"role":"assistant","time":{"completed":123}}"#,
        );
        fx.insert_part("p1", "ses_root", r#"{"type":"text","text":"done"}"#);

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::Waiting);
        assert_eq!(activity.raw_word.as_deref(), Some("text"));
    }

    #[test]
    fn activity_keeps_an_unconfirmed_pending_tool_unknown_without_permission_row() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_part(
            "p1",
            "ses_root",
            r#"{"type":"tool","state":{"status":"pending"}}"#,
        );

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::Unknown);
    }

    #[test]
    fn activity_reads_a_pending_permission_as_needing_you() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.writer
            .execute_batch(
                "CREATE TABLE permission (
                   id TEXT PRIMARY KEY, project_id TEXT NOT NULL, action TEXT NOT NULL,
                   resource TEXT NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL
                 );
                 INSERT INTO permission VALUES ('perm', 'prj', 'read', 'file', 1, 2);",
            )
            .expect("permission");
        fx.insert_part(
            "p1",
            "ses_root",
            r#"{"type":"tool","state":{"status":"running"}}"#,
        );

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::NeedsYou);
        assert_eq!(activity.raw_word.as_deref(), Some("permission"));
    }

    #[test]
    fn activity_does_not_keep_a_rejected_tool_running() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_part(
            "p1",
            "ses_root",
            r#"{"type":"tool","tool":"bash","state":{"status":"error","error":"rejected"}}"#,
        );

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::Waiting);
        assert_eq!(activity.raw_word.as_deref(), Some("tool:error"));
    }

    #[test]
    fn activity_prefers_a_pending_event_over_a_mutable_running_part() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_part(
            "p1",
            "ses_root",
            r#"{"type":"tool","tool":"bash","state":{"status":"running"}}"#,
        );
        fx.writer
            .execute_batch(
                "CREATE TABLE event (
                   id TEXT PRIMARY KEY, aggregate_id TEXT NOT NULL, seq INTEGER NOT NULL,
                   type TEXT NOT NULL, data TEXT NOT NULL
                 );",
            )
            .expect("event table");
        fx.writer
            .execute(
                "INSERT INTO event VALUES (?1, ?2, 1, 'message.part.updated.1', ?3)",
                rusqlite::params![
                    "evt",
                    "ses_root",
                    r#"{"time":1,"part":{"type":"tool","state":{"status":"pending"}}}"#
                ],
            )
            .expect("pending event");

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::NeedsYou);
    }

    #[test]
    fn a_subagent_row_and_an_archived_row_are_both_left_out_of_discovery() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_session(&TestSession::new("ses_child").child_of("ses_root"));
        fx.insert_session(&TestSession::new("ses_gone").archived_at(REAL_TIME_UPDATED_MS));

        let report = fx.adapter().discover_sessions();
        assert!(
            report.problem.is_none(),
            "a healthy store reports no problem"
        );
        assert_eq!(sids(&report), vec!["ses_root".to_string()]);
    }

    #[test]
    fn a_missing_database_is_a_stated_absence_and_is_never_created_by_the_read() {
        let dir = TempDir::new().expect("temp dir");
        let adapter = OpenCodeAdapter::with_root(dir.path());
        let db = adapter.database_path();
        assert!(!db.exists(), "the fixture starts with no database");

        let report = adapter.discover_sessions();
        let problem = report.problem.expect("a missing database is reported");
        assert_eq!(problem.kind, ErrorKind::RootMissing);
        assert_eq!(
            problem.detail,
            ErrorDetail::Path {
                path: db.display().to_string()
            },
            "the owner needs to know WHICH root is missing"
        );
        assert!(report.sessions.is_empty());

        // Invariant 1, asserted rather than assumed: `mode=ro` cannot create a database, and the
        // read must not have left a journal or shared-memory file behind either.
        assert!(
            !db.exists(),
            "the database must still not exist after a read"
        );
        assert!(!db.with_extension("db-wal").exists(), "no WAL was created");
        assert!(!db.with_extension("db-shm").exists(), "no shm was created");

        // The same absence through the per-session path.
        let err = adapter
            .read_metrics("ses_root")
            .expect_err("no database, no metrics");
        assert_eq!(err.kind, ErrorKind::RootMissing);
        assert!(!db.exists(), "the metrics read must not create it either");
    }

    #[test]
    fn the_account_read_names_only_email_so_no_token_can_reach_an_identity_or_an_error() {
        const CANARY: &str = "LEAKCANARY-0123456789";
        let fx = Fixture::new();
        fx.writer.execute_batch(ACCOUNT_DDL).expect("account ddl");
        fx.writer
            .execute(
                "INSERT INTO account (id, email, url, access_token, refresh_token, time_created,
                 time_updated) VALUES ('a1', 'owner@example.com', 'https://example.test', ?1, ?2,
                 1, 2)",
                rusqlite::params![format!("access-{CANARY}"), format!("refresh-{CANARY}")],
            )
            .expect("insert account");
        fx.write_auth(&format!(
            r#"{{"opencode":{{"type":"api","key":"authkey-{CANARY}"}},
                 "opencode-go":{{"type":"api","key":"other-{CANARY}"}}}}"#
        ));

        let identity = fx.adapter().read_identity();
        assert_eq!(identity.label.as_deref(), Some("owner@example.com"));
        assert!(identity.signed_in);
        let providers = identity.providers.clone().expect("providers");
        assert_eq!(providers.len(), 2);
        assert!(providers.iter().all(|p| p.kind == "api"));

        // `Identity` is not `Serialize`; its derived `Debug` is the projection that would carry a
        // stray field, and the two parts that DO serialize are checked through serde.
        let debug = format!("{identity:?}");
        let wire = serde_json::to_string(&providers).expect("providers serialize");
        let problem = serde_json::to_string(&identity.problem.clone().map(|p| p.message))
            .expect("problem serializes");
        for text in [&debug, &wire, &problem] {
            assert!(!text.contains("LEAKCANARY"), "a credential escaped: {text}");
        }
    }

    #[test]
    fn a_millisecond_timestamp_comes_back_as_the_exact_epoch_ms_it_was_written_as() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));

        let report = fx.adapter().discover_sessions();
        let session = report.sessions.first().expect("one session");
        assert_eq!(session.last_active_ms, REAL_TIME_UPDATED_MS);
        assert_eq!(session.first_active_ms, Some(REAL_TIME_CREATED_MS));

        // Unscaled milliseconds land in this decade. Seconds read as ms would land in 1970 and
        // ms read as seconds in the year 58,000 — both are excluded by these bounds.
        assert!(
            (1_700_000_000_000..2_000_000_000_000).contains(&session.last_active_ms),
            "time_updated is epoch milliseconds already, unlike Claude's and Codex's"
        );
        assert!(matches!(
            session.source_signature,
            SourceSignature::OpenCode { time_updated_ms }
                if time_updated_ms == REAL_TIME_UPDATED_MS
        ));
    }

    #[test]
    fn tool_calls_count_only_part_rows_whose_data_json_types_itself_as_tool() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_part("p1", "ses_root", r#"{"type":"tool","tool":"bash"}"#);
        fx.insert_part("p2", "ses_root", r#"{"type":"tool","tool":"read"}"#);
        fx.insert_part("p3", "ses_root", r#"{"type":"step-start"}"#);
        fx.insert_part("p4", "ses_root", r#"{"type":"reasoning"}"#);
        fx.insert_part("p5", "ses_root", r#"{"type":"text","text":"type is tool"}"#);
        fx.insert_part("p6", "ses_other", r#"{"type":"tool"}"#);

        let metrics = fx.adapter().read_metrics("ses_root").expect("metrics");
        assert_eq!(
            metrics.tool_calls, 2,
            "only $.type == 'tool', and only this session's"
        );
    }

    #[test]
    fn assistant_and_user_messages_are_counted_separately_from_their_data_json() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_message("m1", "ses_root", r#"{"role":"assistant"}"#);
        fx.insert_message("m2", "ses_root", r#"{"role":"assistant"}"#);
        fx.insert_message("m3", "ses_root", r#"{"role":"user"}"#);
        fx.insert_message("m4", "ses_other", r#"{"role":"assistant"}"#);

        let metrics = fx.adapter().read_metrics("ses_root").expect("metrics");
        assert_eq!(metrics.api_calls, 2);
        assert_eq!(metrics.user_turns, 1);
    }

    #[test]
    fn the_engines_own_cost_surfaces_and_a_null_cost_is_an_absence_not_a_zero() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_paid").cost(Some(5.823_600_42)));
        let paid = fx.adapter().read_metrics("ses_paid").expect("metrics");
        assert_eq!(paid.provider_cost_usd, Some(5.823_600_42));

        let null = Fixture::with_ddl(SESSION_DDL_NULLABLE_COST);
        null.insert_session(&TestSession::new("ses_free").cost(None));
        let free = null.adapter().read_metrics("ses_free").expect("metrics");
        assert_eq!(
            free.provider_cost_usd, None,
            "a NULL cost is unknown, not $0.00"
        );
    }

    #[test]
    fn a_session_with_no_api_calls_has_undefined_kpis_rather_than_zeroes() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_quiet"));

        let metrics = fx.adapter().read_metrics("ses_quiet").expect("metrics");
        assert_eq!(metrics.api_calls, 0);
        let kpis = metrics.kpis();
        assert_eq!(
            kpis.context_per_call, None,
            "cache_read ÷ 0 calls is undefined"
        );
        assert_eq!(
            kpis.batching_ratio, None,
            "tool_calls ÷ 0 calls is undefined"
        );
        // The one ratio whose denominator is real here still computes.
        assert!(kpis.rewrite_ratio.is_some());
    }

    #[test]
    fn a_missing_expected_column_is_an_unknown_shape_that_names_the_column() {
        let dir = TempDir::new().expect("temp dir");
        let writer = Connection::open(dir.path().join(DB_FILE)).expect("create db");
        writer
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))
            .expect("set wal");
        // The live schema with one column renamed — exactly how an engine's format drifts.
        writer
            .execute_batch(&SESSION_DDL.replace("`tokens_cache_read`", "`tokens_cached`"))
            .expect("drifted ddl");
        writer.execute_batch(MESSAGE_DDL).expect("message ddl");
        writer.execute_batch(PART_DDL).expect("part ddl");

        let adapter = OpenCodeAdapter::with_root(dir.path());
        let report = adapter.discover_sessions();
        let problem = report.problem.expect("drift is reported, not absorbed");
        assert_eq!(problem.kind, ErrorKind::UnknownShape);
        assert_eq!(
            problem.detail,
            ErrorDetail::Fields {
                fields: vec!["session.tokens_cache_read".to_string()]
            }
        );
        assert!(problem.message.contains("tokens_cache_read"));
        assert!(
            report.sessions.is_empty(),
            "no row is rendered from a guessed schema"
        );

        // And the same through the metrics path, which must not return a zeroed Metrics.
        let err = adapter
            .read_metrics("ses_root")
            .expect_err("no guessed number");
        assert_eq!(err.kind, ErrorKind::UnknownShape);
    }

    #[test]
    fn a_root_directory_containing_a_space_still_opens_through_the_uri_form() {
        let outer = TempDir::new().expect("temp dir");
        let root = outer.path().join("open code data");
        std::fs::create_dir_all(&root).expect("mkdir");
        let writer = Connection::open(root.join(DB_FILE)).expect("create db");
        writer
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))
            .expect("set wal");
        writer.execute_batch(SESSION_DDL).expect("session ddl");
        writer.execute_batch(MESSAGE_DDL).expect("message ddl");
        writer.execute_batch(PART_DDL).expect("part ddl");
        writer
            .execute(
                "INSERT INTO session (id, project_id, slug, directory, title, version,
                 time_created, time_updated) VALUES ('ses_space', 'p', 's', '/tmp', 't', '1',
                 ?1, ?2)",
                rusqlite::params![REAL_TIME_CREATED_MS, REAL_TIME_UPDATED_MS],
            )
            .expect("insert");

        let report = OpenCodeAdapter::with_root(&root).discover_sessions();
        assert!(
            report.problem.is_none(),
            "a space in the path is not an error"
        );
        assert_eq!(sids(&report), vec!["ses_space".to_string()]);
    }

    #[test]
    fn the_uri_escapes_the_characters_that_would_change_which_file_is_opened() {
        let path = Path::new("/Users/k/a b/q?x/h#y/50%/opencode.db");
        let live = to_uri(path, Spelling::WalAware);
        assert_eq!(
            live, "file:/Users/k/a%20b/q%3Fx/h%23y/50%25/opencode.db?mode=ro",
            "?, # and % must not survive into the URI unescaped"
        );
        let idle = to_uri(path, Spelling::Immutable);
        assert!(idle.ends_with("?immutable=1"), "the escaping is shared");
        assert_eq!(
            idle.trim_end_matches("?immutable=1"),
            live.trim_end_matches("?mode=ro")
        );
    }

    #[test]
    fn a_wal_beside_the_database_forces_the_wal_aware_spelling_and_its_absence_the_other() {
        let fx = Fixture::new();
        let db = fx.root.join(DB_FILE);
        assert!(
            wal_path(&db).exists(),
            "an open writer keeps a WAL beside it"
        );
        assert_eq!(Spelling::for_database(&db), Spelling::WalAware);

        let closed = fx.quit_engine();
        assert!(
            !wal_path(&closed.db()).exists(),
            "a clean close takes it away"
        );
        assert_eq!(Spelling::for_database(&closed.db()), Spelling::Immutable);

        // The suffix is appended, never substituted: `with_extension` would look for
        // `opencode-wal` in a folder called `opencode.db` and find nothing, every time.
        assert_eq!(
            wal_path(Path::new("/a/opencode.db")),
            PathBuf::from("/a/opencode.db-wal")
        );
    }

    #[test]
    fn a_title_is_tidied_and_an_empty_one_falls_back_to_untitled() {
        let fx = Fixture::new();
        let mut long = TestSession::new("ses_long");
        long.title = format!("  fix\n  the  thing {}  ", "x".repeat(200));
        fx.insert_session(&long);
        let mut blank = TestSession::new("ses_blank");
        blank.title = "   ".to_string();
        fx.insert_session(&blank);

        let report = fx.adapter().discover_sessions();
        let by_id = |sid: &str| {
            report
                .sessions
                .iter()
                .find(|s| s.key.sid == sid)
                .expect("session")
                .clone()
        };
        let tidied = by_id("ses_long");
        assert!(tidied.title.starts_with("fix the thing"));
        assert_eq!(
            tidied.title.chars().count(),
            TITLE_MAX + 1,
            "clipped plus one ellipsis"
        );
        assert_eq!(by_id("ses_blank").title, Session::UNTITLED);
    }

    #[test]
    fn a_row_with_no_working_directory_is_marked_unresumable_with_its_reason() {
        let fx = Fixture::new();
        let mut nowhere = TestSession::new("ses_nowhere");
        nowhere.directory = String::new();
        fx.insert_session(&nowhere);
        fx.insert_session(&TestSession::new("ses_somewhere"));

        let report = fx.adapter().discover_sessions();
        let find = |sid: &str| {
            report
                .sessions
                .iter()
                .find(|s| s.key.sid == sid)
                .expect("session")
                .clone()
        };
        let nowhere = find("ses_nowhere");
        assert!(nowhere.cwd.is_none());
        assert!(!nowhere.resumable);
        assert_eq!(
            nowhere.resume_blocked_reason,
            Some(ResumeBlockedReason::MissingWorkingDirectory)
        );
        let somewhere = find("ses_somewhere");
        assert!(somewhere.resumable);
        assert!(somewhere.git_branch.is_none(), "OpenCode records no branch");
    }

    #[test]
    fn all_sessions_counters_come_back_from_one_pass_matching_the_per_session_read() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_a"));
        fx.insert_session(&TestSession::new("ses_b"));
        fx.insert_session(&TestSession::new("ses_child").child_of("ses_a"));
        fx.insert_message("m1", "ses_a", r#"{"role":"assistant"}"#);
        fx.insert_message("m2", "ses_b", r#"{"role":"user"}"#);
        fx.insert_part("p1", "ses_a", r#"{"type":"tool"}"#);

        let adapter = fx.adapter();
        let all = adapter.read_all_metrics().expect("bulk metrics");
        assert_eq!(all.len(), 2, "subagent rows are not sessions of their own");
        assert_eq!(all["ses_a"], adapter.read_metrics("ses_a"));
        assert_eq!(all["ses_b"], adapter.read_metrics("ses_b"));
        let a = all["ses_a"].clone().expect("ses_a counted");
        assert_eq!(a.api_calls, 1);
        assert_eq!(a.tool_calls, 1);
        assert_eq!(all["ses_b"].clone().expect("ses_b counted").user_turns, 1);
        let span = (REAL_TIME_UPDATED_MS - REAL_TIME_CREATED_MS) as u64;
        assert_eq!(a.duration_ms, Some(span));
        assert_eq!(a.reasoning_tokens, Some(45_592));
        assert_eq!(a.cache_read, 117_139_812);
    }

    #[test]
    fn a_message_row_that_is_not_json_fails_loud_in_the_bulk_path_too_and_not_by_vanishing() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_session(&TestSession::new("ses_fine"));
        fx.insert_message("m1", "ses_root", r#"{"role":"assistant"}"#);
        fx.insert_message("m2", "ses_root", "this is not json");
        fx.insert_message("m3", "ses_fine", r#"{"role":"user"}"#);

        let err = fx
            .adapter()
            .read_metrics("ses_root")
            .expect_err("drift is loud");
        assert_eq!(err.kind, ErrorKind::UnknownShape);
        assert!(err.message.contains("message.data"));

        // The bulk path used to `continue` here, so the row simply was not in the map and the
        // caller could not tell that from a session with nothing to count. It now carries the
        // same error the per-session read gives, which the service renders as Unavailable.
        let all = fx.adapter().read_all_metrics().expect("bulk still works");
        assert_eq!(
            all.len(),
            2,
            "the unreadable session is present, not absent"
        );
        assert_eq!(all["ses_root"], Err(err));
        assert!(
            all["ses_fine"].is_ok(),
            "one bad session does not blank the others"
        );
    }

    #[test]
    fn capacity_without_a_go_credential_is_a_stated_absence() {
        let root = tempfile::tempdir().expect("root");
        let cap = OpenCodeAdapter::with_root(root.path()).read_capacity();
        assert!(!cap.supported, "no Go credential means no Go allowance");
        assert!(
            cap.windows.is_empty(),
            "a zero bar would read as 'all used'"
        );
        assert!(
            cap.problem.is_some(),
            "the missing credential must be explained"
        );
    }

    #[test]
    fn a_missing_auth_file_is_a_stated_absence_of_a_credential() {
        let fx = Fixture::new();
        let identity = fx.adapter().read_identity();
        assert!(!identity.signed_in);
        assert_eq!(
            identity.problem.expect("problem").kind,
            ErrorKind::NoCredential
        );
    }

    #[test]
    fn go_usage_response_becomes_three_capacity_windows() {
        let value = serde_json::json!({
            "usage": {
                "rolling": { "percent": 18, "resetsAt": "2026-09-14T20:00:00.000Z" },
                "weekly": { "percent": 42, "resetsAt": "2026-09-20T00:00:00.000Z" },
                "monthly": { "percent": 7, "resetsAt": "2026-10-01T00:00:00.000Z" }
            }
        });

        let windows = go_capacity(&value, 0).expect("usage shape");
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].name, CapacityWindowName::FiveHour);
        assert_eq!(windows[0].used_pct, 18.0);
        assert_eq!(windows[1].name, CapacityWindowName::Weekly);
        assert_eq!(windows[2].name, CapacityWindowName::Monthly);
        assert!(windows[2].resets_at_ms.is_some());
    }

    #[test]
    fn malformed_go_usage_does_not_render_a_partial_quota() {
        let value = serde_json::json!({
            "usage": {
                "rolling": { "percent": 18, "resetsAt": null },
                "weekly": { "percent": 101, "resetsAt": null },
                "monthly": { "percent": 7, "resetsAt": null }
            }
        });

        assert!(go_capacity(&value, 0).is_err());
    }

    #[test]
    fn an_auth_file_that_is_not_an_object_of_providers_is_an_unknown_shape() {
        let fx = Fixture::new();
        fx.write_auth("[\"opencode\"]");
        let identity = fx.adapter().read_identity();
        assert_eq!(
            identity.problem.expect("problem").kind,
            ErrorKind::UnknownShape
        );
        assert!(identity.providers.is_none());
    }

    #[test]
    fn an_account_table_with_no_rows_leaves_the_label_absent_and_the_identity_intact() {
        let fx = Fixture::new();
        fx.writer.execute_batch(ACCOUNT_DDL).expect("account ddl");
        fx.write_auth(r#"{"opencode":{"type":"api","key":"k"}}"#);

        let identity = fx.adapter().read_identity();
        assert!(identity.signed_in, "auth.json alone decides signed-in");
        assert_eq!(
            identity.label, None,
            "0 account rows on this machine is an absence"
        );
        assert!(identity.problem.is_none());
    }

    /// Real-data smoke test, run on this Mac (the owner's MacBook), 2026-09-13, against the live
    /// `~/.local/share/opencode/opencode.db`: 59 MB with a 3.8 MB WAL and a running engine.
    ///
    /// It proves the WAL was honoured by comparing our count with an independent `sqlite3` read
    /// of the same database — an `immutable=1` open would answer from the last checkpoint and
    /// disagree. Measured that day: 29 session rows, 24 of them roots. Returns early rather than
    /// failing on a machine that has no OpenCode.
    #[test]
    fn the_live_opencode_database_reads_through_its_wal_on_this_mac() {
        let adapter = OpenCodeAdapter::new();
        let db = adapter.database_path();
        if !db.exists() {
            return;
        }

        let report = adapter.discover_sessions();
        assert!(
            report.problem.is_none(),
            "the live schema still matches: {:?}",
            report.problem
        );
        assert!(
            !report.sessions.is_empty(),
            "24 root sessions on this machine, 2026-09-13"
        );

        for session in &report.sessions {
            assert!(!session.key.sid.trim().is_empty());
            assert!(
                (1_600_000_000_000..2_000_000_000_000).contains(&session.last_active_ms),
                "live time_updated is epoch ms: {} on {}",
                session.last_active_ms,
                session.key.sid
            );
        }

        // The independent read. `sqlite3` honours the WAL exactly as we do; a stale image would
        // not agree. Skipped when the CLI is absent rather than failing for its absence.
        let query = "SELECT COUNT(*) FROM session \
                     WHERE parent_id IS NULL AND time_archived IS NULL";
        let out = std::process::Command::new("sqlite3")
            .arg(format!("file:{}?mode=ro", db.display()))
            .arg(query)
            .output();
        let Ok(out) = out else { return };
        if !out.status.success() {
            return;
        }
        let theirs: usize = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("a count");
        assert_eq!(
            report.sessions.len(),
            theirs,
            "our WAL-honouring read must agree with sqlite3's; a session created between the \
             two reads is the only benign way this differs"
        );

        // The database is the owner's live store: prove the read left it exactly as found.
        let before = std::fs::metadata(&db).expect("metadata");
        let _ = adapter.read_all_metrics().expect("live metrics");
        let after = std::fs::metadata(&db).expect("metadata");
        assert_eq!(before.len(), after.len(), "read-only on sources");
    }
    /// The case whose absence let the bug ship: every other fixture here holds its writer open
    /// for the life of the test, which keeps `-shm` alive and hides what an idle engine looks
    /// like. Before the fix, `mode=ro` answered this by writing `-shm` and `-wal` back into the
    /// engine's directory (SQLite 3.46.0) or by failing with `SQLITE_CANTOPEN` and reporting
    /// `RootMissing` about a database that was right there (3.43.2, or an unwritable directory).
    #[test]
    fn a_cleanly_closed_database_still_reads_when_the_engine_is_not_running() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_message("m1", "ses_root", r#"{"role":"assistant"}"#);
        fx.insert_part("p1", "ses_root", r#"{"type":"tool"}"#);
        let closed = fx.quit_engine();

        let report = closed.adapter().discover_sessions();
        assert!(
            report.problem.is_none(),
            "quitting OpenCode does not delete its sessions: {:?}",
            report.problem
        );
        assert_eq!(sids(&report), vec!["ses_root".to_string()]);

        let metrics = closed.adapter().read_metrics("ses_root").expect("metrics");
        assert_eq!(metrics.api_calls, 1);
        assert_eq!(metrics.tool_calls, 1);
        let bulk = closed.adapter().read_all_metrics().expect("bulk metrics");
        assert_eq!(bulk["ses_root"], Ok(metrics));

        // Invariant 1, asserted and not assumed: three reads of the owner's store left the
        // engine's directory exactly as the engine left it.
        assert_eq!(closed.files(), vec![DB_FILE.to_string()]);
    }

    #[test]
    fn a_database_that_exists_but_cannot_be_opened_is_io_and_never_a_missing_root() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        let closed = fx.quit_engine();
        let db = closed.db();
        let restore = std::fs::metadata(&db).expect("metadata").permissions();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");

        let report = closed.adapter().discover_sessions();
        let problem = report
            .problem
            .clone()
            .expect("an unreadable database is reported");
        // SQLite answers `CannotOpen(14)` here just as it does for a database that is not there,
        // and the file is 59 MB of the owner's history in the real case. "OpenCode has no data
        // at <path>" would be a lie that sends them looking for a deleted file.
        assert_eq!(problem.kind, ErrorKind::Io);
        assert_eq!(
            problem.detail,
            ErrorDetail::Path {
                path: db.display().to_string()
            },
            "the path is still named — it is the one fact that makes the message actionable"
        );
        assert!(
            db.exists(),
            "the database this test is about is right there"
        );
        assert_eq!(
            closed.files(),
            vec![DB_FILE.to_string()],
            "a failed read creates nothing"
        );
        std::fs::set_permissions(&db, restore).expect("restore");

        // The same distinction one level up: a directory we may not even `stat` through is not
        // evidence that the database inside it was deleted.
        let dir_restore = std::fs::metadata(&closed.root)
            .expect("metadata")
            .permissions();
        std::fs::set_permissions(&closed.root, std::fs::Permissions::from_mode(0o000))
            .expect("chmod the directory");
        let shut = closed.adapter().discover_sessions();
        let kind = shut.problem.map(|p| p.kind);
        std::fs::set_permissions(&closed.root, dir_restore).expect("restore the directory");
        assert_eq!(
            kind,
            Some(ErrorKind::Io),
            "unreachable is not the same as absent"
        );
    }

    /// The guard on the `immutable=1` spelling, proved by what it would have cost: this WAL
    /// holds the only copy of the session, so an `immutable=1` read answers zero rows.
    #[test]
    fn a_stray_wal_file_keeps_the_read_wal_aware_so_nothing_inside_it_is_skipped() {
        let fx = Fixture::new();
        // Checkpoint the schema into the database, then stop checkpointing: everything after
        // this point lives in the WAL alone, which is the state a running engine is in.
        fx.writer
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                r.get::<_, i64>(0)
            })
            .expect("checkpoint the ddl");
        fx.writer
            .execute_batch("PRAGMA wal_autocheckpoint = 0;")
            .expect("stop checkpointing");
        fx.insert_session(&TestSession::new("ses_in_wal"));

        // Copy the database and its WAL, but not the `-shm`: that is what a crashed engine, or
        // an engine still running elsewhere, leaves a reader to work with.
        let dir = TempDir::new().expect("temp dir");
        let db = dir.path().join(DB_FILE);
        std::fs::copy(fx.root.join(DB_FILE), &db).expect("copy db");
        std::fs::copy(wal_path(&fx.root.join(DB_FILE)), wal_path(&db)).expect("copy wal");
        assert!(
            std::fs::metadata(wal_path(&db)).expect("wal").len() > 0,
            "a WAL with content"
        );

        assert_eq!(Spelling::for_database(&db), Spelling::WalAware);
        let report = OpenCodeAdapter::with_root(dir.path()).discover_sessions();
        assert!(report.problem.is_none(), "{:?}", report.problem);
        assert_eq!(
            sids(&report),
            vec!["ses_in_wal".to_string()],
            "the WAL was honoured"
        );

        // What the forbidden spelling would have answered about the same three files.
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
        let stale = Connection::open_with_flags(to_uri(&db, Spelling::Immutable), flags)
            .expect("immutable opens");
        let rows: i64 = stale
            .query_row("SELECT COUNT(*) FROM session", [], |r| r.get(0))
            .expect("count");
        assert_eq!(
            rows, 0,
            "immutable=1 reads past the WAL — which is why it is guarded"
        );
    }

    // -- The three owner cases, through the shared policy ---------------------------------------

    #[test]
    fn opencode_permits_and_questions_need_the_owner_and_an_abort_hands_the_turn_back() {
        let permission = OpenCodeWaitFacts {
            permission: Some("a pending event".into()),
            ..Default::default()
        };
        let signal = OpenCodeWait::new(&permission)
            .owner_wait()
            .expect("permission");
        assert_eq!(signal.case, OwnerWait::Permission);
        assert_eq!(signal.case.state(), LiveState::NeedsYou);

        let question = OpenCodeWaitFacts {
            question: true,
            ..Default::default()
        };
        let signal = OpenCodeWait::new(&question).owner_wait().expect("question");
        assert_eq!(signal.case, OwnerWait::Question);
        assert_eq!(signal.case.state(), LiveState::NeedsYou);

        let interrupted = OpenCodeWaitFacts {
            interrupted: true,
            ..Default::default()
        };
        let signal = OpenCodeWait::new(&interrupted)
            .owner_wait()
            .expect("interruption");
        assert_eq!(signal.case, OwnerWait::Interruption);
        assert_eq!(signal.case.state(), LiveState::Waiting);
    }

    #[test]
    fn opencode_precedence_is_permission_then_question_then_interruption() {
        let all = OpenCodeWaitFacts {
            permission: Some("a pending event".into()),
            question: true,
            interrupted: true,
        };
        assert_eq!(
            OpenCodeWait::new(&all).owner_wait().unwrap().case,
            OwnerWait::Permission
        );
        let q_and_i = OpenCodeWaitFacts {
            permission: None,
            question: true,
            interrupted: true,
        };
        assert_eq!(
            OpenCodeWait::new(&q_and_i).owner_wait().unwrap().case,
            OwnerWait::Question
        );
    }

    /// **The gap, pinned.** A live approval that OpenCode never writes a pending event for — an
    /// `external_directory` ask, measured 2026-09-18 — leaves no trace the adapter can read, so
    /// the policy answers `None` rather than inventing a permission. The turn reading then says
    /// what the newest part says; it does not claim the owner is needed.
    #[test]
    fn a_permission_with_no_persisted_trace_is_a_stated_absence_not_a_guess() {
        let facts = OpenCodeWaitFacts::default();
        assert!(OpenCodeWait::new(&facts).owner_wait().is_none());
    }

    #[test]
    fn an_aborted_turn_reads_waiting_even_though_the_part_is_still_running() {
        let fx = Fixture::new();
        fx.insert_session(&TestSession::new("ses_root"));
        fx.insert_message(
            "msg",
            "ses_root",
            r#"{"role":"assistant","time":{"created":1,"completed":2},"error":{"name":"MessageAbortedError","data":{"message":"Aborted"}}}"#,
        );
        fx.insert_part(
            "p1",
            "ses_root",
            r#"{"type":"tool","state":{"status":"running"}}"#,
        );

        let activity = fx
            .adapter()
            .read_activity("ses_root")
            .expect("activity should be readable");
        assert_eq!(activity.state, LiveState::Waiting);
        assert_eq!(activity.raw_word.as_deref(), Some("MessageAbortedError"));
    }
}
