//! Live status — which sessions have an engine process open on this host **right now**, and what
//! that process is doing.
//!
//! Pigeon is the first thing in this family to read *process* facts; Demo Studio reads none,
//! so there was nothing to port. Every rule below was established by measuring this Mac on
//! 2026-09-13, and the measurements live next to the code they justify rather than in a document
//! that will drift away from it.
//!
//! # The vocabulary is deliberately small
//!
//! [`LiveState::Running`], [`LiveState::Waiting`], [`LiveState::NeedsYou`], [`LiveState::Unknown`]
//! — and a session with no live process gets **no observation at all**. It is absent from
//! [`StatusSnapshot::live`],
//! counted nowhere, and badged nothing. `Finished` is a session *display* state derived from that
//! absence; it is never a live observation, which is why [`LiveState`] has no such variant.
//!
//! `Unknown` is honest, not lazy: a process is present and the engine publishes nothing we can
//! read. An `Unknown` we can explain is worth more than a `Running` we guessed.
//!
//! # What each engine publishes, measured on this Mac 2026-09-13
//!
//! **Claude Code — the richest source.** `~/.claude/sessions/<pid>.json` exists per running CLI
//! and is a flat object (no nesting) carrying `pid`, `sessionId`, `cwd`, `status`,
//! `statusUpdatedAt`, `startedAt`, `name`, `version` and a `messagingSocketPath`. Four files were
//! present; the status words across them were `busy` (1) and `idle` (3). `needs_input` is
//! documented by the CLI and is mapped here, but was not observed. The file names its own session
//! outright — `sessionId` is the 36-char uuid — so no cwd matching is ever needed, and the
//! `.key` sibling file next to each `.json` is never opened.
//!
//! `claude agents --json` also exists and answers in ~182 ms. **We deliberately do not call it.**
//! 182 ms per poll is two orders of magnitude above the 0.03 ms a `read_to_string` of a 558-byte
//! file costs, and shelling out to an engine is a heavier commitment than reading a file it
//! already wrote: it can prompt, it can block on the network, and its argv/exit contract is the
//! engine's to change. The file is the cheaper and less entangled source, so the file wins.
//!
//! **Codex — the cleanest attribution of the three.** A live Codex process holds
//! `~/.codex/thread-writer-locks/<thread>.lock` open, and the lock's filename *is* the thread id.
//! Measured: `lsof -c codex -Fpn` took **0.04 s** and showed pid 26691 holding
//! `…/01a08d01-cea6-7d43-8552-dfbb944db822.lock` and pid 52363 holding
//! `…/01a0963c-e907-7372-85be-d56b6669b13a.lock`, each alongside that thread's own rollout
//! `.jsonl`. `lsof +D <locks dir>` answers the same question in 0.36 s — 9× slower, because `+D`
//! walks — so the scoped `-c codex` form is the one used here.
//!
//! Only the *binary* holds the lock: pid 26689 (`node …/codex resume`) is 26691's parent and holds
//! nothing, so there is no risk of one lock producing two owners.
//!
//! **OpenCode — attributable by cwd, with activity owned by its SQLite adapter.** See
//! [`OpenCodeAttributor`].
//!
//! # Never guess an attribution
//!
//! A cwd match is not proof: two sessions can run in one folder, and on this machine two live
//! `opencode` processes sat in two different project folders while four `claude` processes sat in
//! four. If two processes could be the session's, that is [`ErrorKind::ProcessAmbiguous`] and
//! [`StatusService::stop_session`] stops nothing.
//!
//! # Evidence is the product
//!
//! Every observation carries short sentences naming what was actually seen. **No evidence string
//! ever contains a command line** — argv can hold a prompt, and prompts are private. The cheapest
//! way to guarantee that is to never read one: the real probe asks `ps` for `pid,ppid,comm` and
//! never for `args`. [`ProcessFact::command`] exists only so a test can prove that even a probe
//! that hands us a command line cannot get it into evidence.
//!
//! # Threading
//!
//! Every method does filesystem reads and subprocess launches, so all of it is synchronous and
//! callable from inside `spawn_blocking`. **No lock is held across a subprocess launch or a
//! filesystem walk** — Demo Studio's invariant 10, and this is the easiest file in the project
//! to break it in. The snapshot cache is a short-TTL [`Cached`], whose std mutex is held only for
//! one read or one store; the refresh runs entirely outside it.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crate::adapters::wait::WaitPolicy;
use crate::adapters::{claude, codex};
use crate::api::errors::{EngineError, ErrorDetail, ErrorKind};
use crate::cache::Cached;
use crate::domain::{
    LiveObservation, LiveState, ProcessPresence, ProviderId, SessionKey, StatusSnapshot,
};
use crate::services::codex_hooks;
use crate::util::{now_ms, tidy_title};

/// Absolute paths of the three status sources. Parameterised so tests can point at a `TempDir`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusRoots {
    /// `~/.claude/sessions` — one `<pid>.json` per running Claude Code CLI.
    pub claude_sessions: PathBuf,
    /// `~/.claude/projects` — depth-one Claude transcripts, used for turn liveness.
    pub claude_projects: PathBuf,
    /// `~/.codex/thread-writer-locks` — one `<thread>.lock` per live Codex thread.
    pub codex_locks: PathBuf,
    /// `~/.codex/sessions` — the rollout tree, read tail-only and never written.
    pub codex_sessions: PathBuf,
}

impl StatusRoots {
    /// The real roots on this host. A machine with no home directory yields paths that do not
    /// exist, which surfaces as [`ErrorKind::RootMissing`] — a stated absence, never a panic.
    pub fn real() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            claude_sessions: home.join(".claude").join("sessions"),
            claude_projects: home.join(".claude").join("projects"),
            codex_locks: home.join(".codex").join("thread-writer-locks"),
            codex_sessions: home.join(".codex").join("sessions"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Probe seams
// ---------------------------------------------------------------------------------------------

/// One live process, as much of it as we are willing to look at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessFact {
    pub pid: u32,
    pub ppid: u32,
    /// The `comm` field. `ps` gives a bare name for some binaries (`claude`, `opencode`) and a
    /// full path for others — on this Mac pid 52363's `comm` was
    /// `/Users/…/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex`. Only [`Self::name`]
    /// (the basename) ever reaches evidence.
    pub comm: String,
    /// **Never populated by [`PsProbe`].** It exists so a test can hand the service a command
    /// line containing a prompt and assert it appears in no evidence string. Nothing in this file
    /// reads it; that is the point.
    pub command: Option<String>,
}

impl ProcessFact {
    /// The basename of `comm`, which is all any evidence sentence is allowed to say about what a
    /// process *is*.
    pub fn name(&self) -> &str {
        self.comm.rsplit(['/', '\\']).next().unwrap_or(&self.comm)
    }
}

/// Why a host probe did not answer. Typed, so no source error's text can be formatted into an
/// `EngineError` (`api::errors` rule 1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeFailure {
    /// The tool could not be launched at all.
    Spawn,
    /// The tool outlived its budget and was killed. Never let a hung `ps` wedge the poll.
    Timeout,
    /// It ran and produced nothing usable.
    Output,
}

/// Proves a pid is alive and names its parent. One call answers for the whole host.
pub trait ProcessProbe: Send + Sync {
    fn snapshot(&self) -> Result<Vec<ProcessFact>, ProbeFailure>;
}

/// Which files a named program holds open. This is how Codex and OpenCode are found at all.
pub trait OpenFileProbe: Send + Sync {
    /// Absolute paths held open by processes whose command matches `program`, as `(pid, path)`.
    fn open_files(&self, program: &str) -> Result<Vec<(u32, PathBuf)>, ProbeFailure>;

    /// Each matching process's working directory.
    fn working_dirs(&self, program: &str) -> Result<Vec<(u32, PathBuf)>, ProbeFailure>;
}

/// Which signal was sent. Nothing in the *status* path ever calls this; only
/// [`StatusService::stop_session`] does, and only after proving ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopSignal {
    Term,
    Kill,
}

impl StopSignal {
    fn flag(self) -> &'static str {
        match self {
            StopSignal::Term => "-TERM",
            StopSignal::Kill => "-KILL",
        }
    }
}

pub trait Signaller: Send + Sync {
    fn signal(&self, pid: u32, signal: StopSignal) -> Result<(), ProbeFailure>;
}

/// The seam the OpenCode adapter fills in, and the reason this file opens no database.
///
/// **Why a seam and not a query.** OpenCode's session store is
/// `~/.local/share/opencode/opencode.db`; the OpenCode adapter owns that connection, and two
/// owners of one SQLite handle is how `database is locked` reaches production. Measured
/// 2026-09-13: a live `opencode` process holds exactly `opencode.db`, its `-wal`, its `-shm` and
/// `log/opencode.log` — **no per-session file, no unix socket, no listening port**. Its cwd is the
/// project folder, and a cwd is not proof of a session because two sessions can run in one folder.
///
/// So OpenCode is **not attributable to a session from host facts alone**. Without an attributor
/// this service says so ([`ErrorKind::Unsupported`], raised only when OpenCode processes are
/// actually live) rather than inventing a key or emitting a keyless `Unknown`.
pub trait OpenCodeAttributor: Send + Sync {
    /// Name the session behind each live process. The adapter is expected to read its own
    /// database — assistant-message completion, and a pending row in the `permission` table, which
    /// is strong evidence of [`LiveState::NeedsYou`]. A completed assistant turn is
    /// [`LiveState::Waiting`].
    fn attribute(
        &self,
        processes: &[OpenCodeProcess],
    ) -> Result<Vec<OpenCodeAttribution>, EngineError>;
}

/// What the host knows about one live `opencode` process. Handed to an [`OpenCodeAttributor`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCodeProcess {
    pub pid: u32,
    pub cwd: Option<PathBuf>,
    /// What we saw, ready to be carried into the resulting observation.
    pub evidence: Vec<String>,
}

/// One attributed OpenCode session. The adapter owns the state decision because it owns the only
/// source that can make it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCodeAttribution {
    /// None when several OpenCode processes share a directory. The session can be shown as live,
    /// but no individual process may be stopped from that ambiguous evidence.
    pub pid: Option<u32>,
    pub sid: String,
    pub state: LiveState,
    pub since_ms: Option<i64>,
    pub raw_word: Option<String>,
    pub evidence: Vec<String>,
    pub active_subagents: u32,
}

// ---------------------------------------------------------------------------------------------
// Real probes
// ---------------------------------------------------------------------------------------------

const PS: &str = "/bin/ps";
const LSOF: &str = "/usr/sbin/lsof";
const KILL: &str = "/bin/kill";

/// Budget for one host probe. Measured on this Mac 2026-09-13: `ps -axo` over 575 processes took
/// 0.03 s and `lsof -c codex -Fpn` took 0.04 s — so 3 s is ~75× the observed cost and only fires
/// when something is genuinely wedged (an unresponsive NFS mount is `lsof`'s classic hang).
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Liveness from `ps`, in one call for the whole host.
pub struct PsProbe;

impl ProcessProbe for PsProbe {
    fn snapshot(&self) -> Result<Vec<ProcessFact>, ProbeFailure> {
        // One listing beats one spawn per pid: 575 processes in 30 ms, measured. It is also more
        // robust — `ps -p <list>` aborts the *whole* call with "process id too large" if any one
        // id is out of range, which a stale status file can easily supply.
        //
        // `comm` and never `args`: argv can carry a prompt, and the only airtight way to keep a
        // prompt out of evidence is to never read one.
        let raw = run_capture(PS, &["-axo", "pid=,ppid=,comm="], PROBE_TIMEOUT)?;
        Ok(parse_ps(&raw))
    }
}

/// Open files and working directories from `lsof`, always scoped to one program name.
pub struct LsofProbe;

impl OpenFileProbe for LsofProbe {
    fn open_files(&self, program: &str) -> Result<Vec<(u32, PathBuf)>, ProbeFailure> {
        // Scoped by `-c <program>`, never a bare `lsof`: unscoped it walks every process on the
        // machine. `-F pn` is the machine-readable form (one `p<pid>` line, then `n<path>` lines).
        let raw = run_capture(LSOF, &["-c", program, "-Fpn"], PROBE_TIMEOUT)?;
        Ok(parse_lsof(&raw))
    }

    fn working_dirs(&self, program: &str) -> Result<Vec<(u32, PathBuf)>, ProbeFailure> {
        let raw = run_capture(
            LSOF,
            &["-c", program, "-a", "-d", "cwd", "-Fpn"],
            PROBE_TIMEOUT,
        )?;
        Ok(parse_lsof(&raw))
    }
}

/// Sends a signal through `/bin/kill`. `libc` is not a dependency of this crate and this lane may
/// not add one, so `kill(2)` is reached through the binary that already wraps it.
pub struct PosixSignaller;

impl Signaller for PosixSignaller {
    fn signal(&self, pid: u32, signal: StopSignal) -> Result<(), ProbeFailure> {
        // The floor lives here, in the one component that cannot afford to trust its caller.
        // `/bin/kill -TERM 0` signals the caller's **own** process group — measured on this Mac
        // 2026-09-13: `/bin/kill -0 0` exits 0 — so a 0 arriving here would take Pigeon and
        // every process sharing its group. Pid 1 is launchd (`/bin/kill -0 1` exits non-zero,
        // EPERM) and is no session's engine either.
        //
        // Unreachable from the observation paths, which all gate on `table.alive(pid)`, and
        // `ps -axo pid=` lists neither number. Guarded anyway: the check costs one comparison
        // and the mistake it prevents cannot be undone. `pid` is a `u32`, so kill's other
        // dangerous spelling — `-<pgid>`, a whole process group — cannot be formed at all.
        if pid < 2 {
            // Nothing was launched, which is exactly what `Spawn` says.
            return Err(ProbeFailure::Spawn);
        }
        let pid = pid.to_string();
        run_capture(KILL, &[signal.flag(), &pid], PROBE_TIMEOUT).map(|_| ())
    }
}

/// Run a command and capture stdout, or give up.
///
/// The reader runs on its own thread and reports through a channel, so the timeout is a
/// `recv_timeout` rather than a `try_wait` poll. That ordering matters: polling `try_wait` while
/// leaving a pipe undrained deadlocks the moment the child's output exceeds the pipe buffer, which
/// `lsof` on a busy host will do.
fn run_capture(program: &str, args: &[&str], timeout: Duration) -> Result<String, ProbeFailure> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ProbeFailure::Spawn)?;
    let mut out = child.stdout.take().ok_or(ProbeFailure::Output)?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    match rx.recv_timeout(timeout) {
        Ok(buf) => {
            let _ = child.wait();
            Ok(String::from_utf8_lossy(&buf).into_owned())
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(ProbeFailure::Timeout)
        }
    }
}

/// `  3975  3870 claude` → one fact. `comm` is the remainder of the line, so a path with an
/// embedded space re-joins with single spaces; only the basename is ever used, so that is
/// harmless.
fn parse_ps(raw: &str) -> Vec<ProcessFact> {
    let mut facts = Vec::new();
    for line in raw.lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else {
            continue;
        };
        let comm = parts.collect::<Vec<_>>().join(" ");
        facts.push(ProcessFact {
            pid,
            ppid,
            comm,
            command: None,
        });
    }
    facts
}

/// `lsof -F` output: a `p<pid>` line opens a process block, each `n<path>` line names a file in
/// it. Every other tag (`f`, `t`, …) is ignored rather than guessed at.
fn parse_lsof(raw: &str) -> Vec<(u32, PathBuf)> {
    let mut out = Vec::new();
    let mut pid: Option<u32> = None;
    for line in raw.lines() {
        let mut chars = line.chars();
        let Some(tag) = chars.next() else { continue };
        let rest = chars.as_str();
        match tag {
            'p' => pid = rest.trim().parse::<u32>().ok(),
            'n' => {
                if let Some(pid) = pid {
                    if rest.starts_with('/') {
                        out.push((pid, PathBuf::from(rest)));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------------------------

/// What one refresh produced. A provider's status-source failure lands in `problems`; it is
/// **never** converted into a fake `Unknown` session, because a badge the owner cannot trace is
/// worse than a stated absence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusReport {
    pub snapshot: StatusSnapshot,
    pub problems: Vec<EngineError>,
}

/// One provider's contribution: rows, plus a problem that travels *with* them so one broken
/// engine never blanks the other two. Same shape as `adapters::ProviderSessionReport`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderObservations {
    pub live: Vec<LiveObservation>,
    pub problem: Option<EngineError>,
}

/// A process we saw, could not prove belonged to the session, and therefore did **not** stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmbiguousMatch {
    pub pid: u32,
    /// Why it was left alone. Never a command line.
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoppedProcess {
    pub pid: u32,
    /// The signal that actually ended it.
    pub signal: StopSignal,
    pub evidence: Vec<String>,
}

/// The outcome of [`StatusService::stop_session`]. `already_stopped` with an empty `stopped` is a
/// success, not an error — the command is idempotent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StopOutcome {
    pub stopped: Vec<StoppedProcess>,
    pub already_stopped: bool,
    pub ambiguous: Vec<AmbiguousMatch>,
}

// ---------------------------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------------------------

/// How long a snapshot stays fresh. One refresh costs ~0.1 s of subprocess time (`ps` 0.03 s +
/// two scoped `lsof` calls at ~0.04 s each, measured 2026-09-13), so a second caller arriving
/// inside the same UI tick should share the first one's answer rather than pay it again. One
/// second is also under the interval at which a badge change is perceptible.
const SNAPSHOT_TTL: Duration = Duration::from_secs(1);

/// How long SIGTERM gets before SIGKILL.
///
/// Measured on this Mac 2026-09-13: the live Codex rollout was **9,897,097 bytes**, and its writer
/// holds both that append-only `.jsonl` and its `thread-writer-locks/<thread>.lock` open. A clean
/// SIGTERM has to append a `turn_aborted` record — there were 13 of them in that one rollout, so
/// the path is real and exercised — fsync an append, and release the lock. That is one fsync, not
/// a rewrite. Claude Code additionally removes its `~/.claude/sessions/<pid>.json` and the
/// `/tmp/cc-socks/<pid>.sock` named in it.
///
/// Two seconds is ~20× the whole measured poll cost, which leaves room for a loaded disk, and is
/// still short enough that the owner does not conclude Stop did nothing. Escalating sooner risks
/// the one outcome we must never cause: a half-written transcript, which *is* the owner's record.
const STOP_GRACE: Duration = Duration::from_millis(2_000);
const STOP_POLL: Duration = Duration::from_millis(100);

/// Reads host process facts and turns them into live observations.
///
/// Synchronous and cheap by construction, so it can be called from `spawn_blocking`.
pub struct StatusService {
    roots: StatusRoots,
    process: Arc<dyn ProcessProbe>,
    files: Arc<dyn OpenFileProbe>,
    signaller: Arc<dyn Signaller>,
    attributor: Option<Arc<dyn OpenCodeAttributor>>,
    /// Where the installed Codex hook appends. A field rather than a call to
    /// [`codex_hooks::events_path`] inside the pass so a test can point it at its own temp file
    /// instead of reading the owner's real event log.
    codex_events: PathBuf,
    cache: Cached<StatusReport>,
    stop_grace: Duration,
}

impl Default for StatusService {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusService {
    /// The real roots and the real probes.
    pub fn new() -> Self {
        Self::with_roots(StatusRoots::real())
    }

    /// Given roots, real probes. Tests point this at a `TempDir` and then swap the probes.
    pub fn with_roots(roots: StatusRoots) -> Self {
        Self {
            roots,
            process: Arc::new(PsProbe),
            files: Arc::new(LsofProbe),
            signaller: Arc::new(PosixSignaller),
            attributor: None,
            codex_events: codex_hooks::events_path(),
            cache: Cached::new(SNAPSHOT_TTL),
            stop_grace: STOP_GRACE,
        }
    }

    /// Point the Codex hook reader at a specific event file.
    pub fn with_codex_events(mut self, path: impl Into<PathBuf>) -> Self {
        self.codex_events = path.into();
        self
    }

    pub fn with_probes(
        mut self,
        process: Arc<dyn ProcessProbe>,
        files: Arc<dyn OpenFileProbe>,
        signaller: Arc<dyn Signaller>,
    ) -> Self {
        self.process = process;
        self.files = files;
        self.signaller = signaller;
        self
    }

    /// Install the OpenCode seam. Until one is installed, OpenCode contributes no rows.
    pub fn with_attributor(mut self, attributor: Arc<dyn OpenCodeAttributor>) -> Self {
        self.attributor = Some(attributor);
        self
    }

    pub fn with_stop_grace(mut self, grace: Duration) -> Self {
        self.stop_grace = grace;
        self
    }

    /// The cached snapshot, refreshed when the TTL has passed.
    ///
    /// Two callers arriving on an expired TTL may both refresh. That is deliberate: the
    /// alternative is holding a lock across two `lsof` launches and a directory read, which is
    /// exactly the compound hold Demo Studio's invariant 10 forbids. A duplicated refresh
    /// costs 0.1 s of subprocess time; a lock held across one starves every other thread.
    pub fn snapshot(&self) -> StatusReport {
        if let Some(cached) = self.cache.fresh() {
            return cached;
        }
        self.refresh()
    }

    /// Just the projection, for a caller that renders badges and has nowhere to put a problem.
    /// Prefer [`Self::snapshot`]: a dropped `problems` list is how "nothing is running" comes to
    /// mean "we could not look".
    pub fn live(&self) -> StatusSnapshot {
        self.snapshot().snapshot
    }

    /// Observe now, ignoring the TTL, and store the result.
    ///
    /// **Stored under the moment it observed, not the moment it arrived.** [`Self::snapshot`]
    /// deliberately lets two callers refresh at once rather than hold a lock across two `lsof`
    /// launches, so a slow pass can finish *after* a fast one that started later. Arrival order
    /// would then let the older world overwrite the newer one stamped as current, and a session
    /// that started in between would vanish from Live for a whole extra poll. A producer
    /// serialized by the refresh mutex can use [`Cached::store`] and always win; this one is
    /// concurrent on purpose, and that is exactly what makes the guard necessary.
    pub fn refresh(&self) -> StatusReport {
        let report = self.observe();
        // Stamped when the pass began, which is the world it describes.
        let observed_at_ms = report.snapshot.generated_at_ms;
        self.cache.store_observed(report.clone(), observed_at_ms);
        report
    }

    /// Drop the cached snapshot. Called after a stop, so the next read cannot show a badge for a
    /// process we just ended.
    pub fn invalidate(&self) {
        self.cache.clear();
    }

    /// One full pass over every provider. No lock is held anywhere in here.
    fn observe(&self) -> StatusReport {
        let mut report = StatusReport {
            snapshot: StatusSnapshot::default(),
            problems: vec![],
        };
        report.snapshot.generated_at_ms = now_ms();

        // Liveness first, once, for all three providers. If `ps` cannot answer we cannot prove any
        // pid is alive, and an unverified observation is exactly the stale `Running` this service
        // exists to avoid — so we state the failure and return nothing.
        let table = match self.process.snapshot() {
            Ok(facts) => facts,
            Err(failure) => {
                report.problems.push(probe_error(None, failure, PS));
                return report;
            }
        };
        let table = ProcessTable::new(table);

        let mut raw = Vec::new();
        for provider in ProviderId::ALL {
            match self.observe_provider(provider, &table) {
                Ok(obs) => {
                    raw.extend(obs.live);
                    if let Some(problem) = obs.problem {
                        report.problems.push(problem);
                    }
                }
                Err(error) => report.problems.push(error),
            }
        }
        report.snapshot.live = collapse(raw);
        report
    }

    fn observe_provider(
        &self,
        provider: ProviderId,
        table: &ProcessTable,
    ) -> Result<ProviderObservations, EngineError> {
        match provider {
            ProviderId::ClaudeCode => self.observe_claude(table),
            ProviderId::Codex => self.observe_codex(table),
            ProviderId::OpenCode => self.observe_opencode(table),
        }
    }

    // -- Claude Code ---------------------------------------------------------------------------

    /// One `<pid>.json` per running CLI. The file names its own session, so attribution is exact
    /// and no cwd matching is needed.
    fn observe_claude(&self, table: &ProcessTable) -> Result<ProviderObservations, EngineError> {
        let root = &self.roots.claude_sessions;
        let entries = std::fs::read_dir(root)
            .map_err(|e| EngineError::from_io(ProviderId::ClaudeCode, &e, root))?;

        let mut out = ProviderObservations::default();
        let mut drifted: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            // The `.key` sibling next to each `.json` is the CLI's own peer material. It is never
            // opened — not to parse, not to check. Only `<stem>.json` is read.
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                drifted.push("json".into());
                continue;
            };
            match claude_observation(&path, &value, table, &self.roots.claude_projects) {
                Ok(Some(obs)) => out.live.push(obs),
                // Alive-but-unreadable is not an error; a dead pid simply yields nothing.
                Ok(None) => {}
                Err(missing) => drifted.push(missing),
            }
        }
        if !drifted.is_empty() {
            // Fail loud on an unknown shape (Studio invariant 2): the rows we *could* read still
            // travel, and the drift is stated rather than swallowed.
            out.problem = Some(EngineError::unknown_shape(
                ProviderId::ClaudeCode,
                &["pid", "sessionId", "status"],
            ));
        }
        Ok(out)
    }

    // -- Codex ---------------------------------------------------------------------------------

    /// A held `thread-writer-locks/<thread>.lock` is the attribution; the rollout's tail is the
    /// state.
    fn observe_codex(&self, table: &ProcessTable) -> Result<ProviderObservations, EngineError> {
        let root = &self.roots.codex_locks;
        if !root.is_dir() {
            return Err(EngineError::root_missing(ProviderId::Codex, root));
        }
        let open = self
            .files
            .open_files(ProviderId::Codex.program())
            .map_err(|f| probe_error(Some(ProviderId::Codex), f, LSOF))?;

        // Group one pass of `lsof` output by pid: the lock names the thread, and the same pid's
        // rollout under `codex_sessions` supplies the turn marker.
        let mut by_pid: BTreeMap<u32, CodexFiles> = BTreeMap::new();
        for (pid, path) in open {
            if path.starts_with(root) {
                if let Some(thread) = lock_thread_id(&path) {
                    by_pid.entry(pid).or_default().lock = Some(CodexLock { thread, path });
                }
            } else if path.starts_with(&self.roots.codex_sessions)
                && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
            {
                by_pid.entry(pid).or_default().rollout = Some(path);
            }
        }

        // Read once for the whole pass. An empty map is "no hook evidence", never a claim that
        // nothing is running — every reading below is only ever *overridden* by what it finds.
        let hook_events = codex_hooks::latest_by_session(&self.codex_events);

        let mut out = ProviderObservations::default();
        for (pid, CodexFiles { lock, rollout }) in by_pid {
            let Some(CodexLock {
                thread,
                path: lock_path,
            }) = lock
            else {
                continue;
            };
            let Some(fact) = table.alive(pid) else {
                continue;
            };

            let mut evidence = vec![
                note(format!("pid {pid} holds {}", lock_path.display())),
                note(format!("pid {pid} is alive as \"{}\"", fact.name())),
            ];
            // The lock proves a process is *attached*. Whether it is working or waiting comes from
            // the rollout, and if there is no rollout the honest answer is Unknown.
            let turn = rollout.as_deref().and_then(codex::codex_turn);
            let since_ms = turn.as_ref().and_then(|(_, ts)| *ts);
            match turn.as_ref().map(|(marker, _)| marker) {
                Some(codex::CodexTurn::Open) => {
                    evidence.push(note(
                        "its rollout's last turn record is task_started".into(),
                    ));
                }
                Some(codex::CodexTurn::Closed(word)) => {
                    evidence.push(note(format!("its rollout's last turn record is {word}")));
                }
                None => evidence.push(note(
                    "no turn record in the tail of its rollout, so its state is unreadable".into(),
                )),
            }
            // **The rollout cannot see an approval.** A Codex paused on an approval prompt still
            // has an open `task_started`, so the tail above says Running for a session that is
            // waiting on a human. The hook is the only source that knows, and it is allowed to
            // overrule the tail — but only a RUNNING reading, so a hook line left over from an
            // earlier turn can never resurrect a session Codex has already finished.
            let hook_event = hook_events
                .get(&thread)
                .map(|event| event.event_name.as_str());
            let wait = codex::CodexWait::new(turn.as_ref().map(|(marker, _)| marker), hook_event);
            let (state, raw_word) = match wait.owner_wait() {
                Some(signal) => {
                    if let Some(reason) = signal.evidence {
                        evidence.push(note(reason));
                    }
                    (signal.case.state(), signal.raw_word)
                }
                None => (wait.turn_state(), wait.turn_word()),
            };
            let active_subagents =
                codex::codex_subagent_facts(&self.roots.codex_sessions, &thread).active;
            let state = if state == LiveState::Waiting && active_subagents > 0 {
                evidence.push(note(format!(
                    "Codex is idle while {active_subagents} delegated subagent(s) are still active"
                )));
                LiveState::Delegating
            } else {
                state
            };
            out.live.push(LiveObservation {
                key: SessionKey::new(ProviderId::Codex, thread),
                process: ProcessPresence::Present,
                state,
                since_ms,
                raw_word,
                evidence,
                pid: Some(pid),
                console_id: None,
                active_subagents,
                observed_at_ms: now_ms(),
            });
        }
        Ok(out)
    }

    // -- OpenCode ------------------------------------------------------------------------------

    /// Process facts only. Without an [`OpenCodeAttributor`] there is no session id to key an
    /// observation on, so live processes become a stated [`ErrorKind::Unsupported`] rather than
    /// rows we cannot name.
    fn observe_opencode(&self, table: &ProcessTable) -> Result<ProviderObservations, EngineError> {
        let dirs = self
            .files
            .working_dirs(ProviderId::OpenCode.program())
            .map_err(|f| probe_error(Some(ProviderId::OpenCode), f, LSOF))?;

        let mut processes: Vec<OpenCodeProcess> = Vec::new();
        let mut seen: Vec<u32> = Vec::new();
        for (pid, cwd) in dirs {
            if seen.contains(&pid) {
                continue;
            }
            let Some(fact) = table.alive(pid) else {
                continue;
            };
            seen.push(pid);
            processes.push(OpenCodeProcess {
                pid,
                cwd: Some(cwd.clone()),
                evidence: vec![
                    note(format!("pid {pid} is alive as \"{}\"", fact.name())),
                    // Stated as what it is: a folder, not a session. Two sessions can share one.
                    note(format!(
                        "pid {pid}'s working directory is {}",
                        cwd.display()
                    )),
                ],
            });
        }

        let mut out = ProviderObservations::default();
        if processes.is_empty() {
            return Ok(out);
        }
        let Some(attributor) = self.attributor.as_ref() else {
            out.problem = Some(EngineError::of(
                ProviderId::OpenCode,
                ErrorKind::Unsupported,
            ));
            return Ok(out);
        };
        let attributions = attributor.attribute(&processes)?;
        let mut stray = false;
        for attribution in attributions {
            if attribution.sid.trim().is_empty() {
                continue;
            }
            let mut evidence = match attribution.pid {
                Some(pid) => {
                    let Some(proved) = processes.iter().find(|p| p.pid == pid) else {
                        stray = true;
                        continue;
                    };
                    proved.evidence.clone()
                }
                None => vec!["OpenCode has multiple live processes in this directory; the exact process-to-session mapping is ambiguous".into()],
            };
            evidence.extend(attribution.evidence.into_iter().map(note));
            out.live.push(LiveObservation {
                key: SessionKey::new(ProviderId::OpenCode, attribution.sid),
                process: ProcessPresence::Present,
                state: attribution.state,
                since_ms: attribution.since_ms,
                raw_word: attribution.raw_word,
                evidence,
                active_subagents: attribution.active_subagents,
                pid: attribution.pid,
                console_id: None,
                observed_at_ms: now_ms(),
            });
        }
        if stray {
            // Fail loud on an unknown shape (Studio invariant 2). The rows we could prove still
            // travel; the seam's breach of contract is stated rather than silently dropped.
            out.problem = Some(EngineError::unknown_shape(ProviderId::OpenCode, &["pid"]));
        }
        Ok(out)
    }

    // -- Stop ----------------------------------------------------------------------------------

    /// Stop every process that can be **proved** to belong to `key`, including ones Pigeon never
    /// started.
    ///
    /// Proof is the same evidence the badge is built from — a Claude status file that names the
    /// session id, or a held Codex writer lock whose filename *is* the thread. A cwd match is not
    /// proof and an executable name is not proof, so neither is ever used here.
    ///
    /// The snapshot cache is bypassed: a badge may be a second stale, a kill decision may not.
    pub fn stop_session(&self, key: &SessionKey) -> Result<StopOutcome, EngineError> {
        if !key.is_valid() {
            return Err(EngineError::of(key.provider_id, ErrorKind::Path));
        }
        let facts = self
            .process
            .snapshot()
            .map_err(|f| probe_error(Some(key.provider_id), f, PS))?;
        let table = ProcessTable::new(facts);
        let observed = self.observe_provider(key.provider_id, &table)?;
        let mine: Vec<&LiveObservation> = observed.live.iter().filter(|o| &o.key == key).collect();

        if mine.is_empty() {
            // We looked and found nothing — unless looking itself failed, in which case "already
            // stopped" would be a claim we cannot support.
            if let Some(problem) = observed.problem {
                return Err(problem);
            }
            self.invalidate();
            return Ok(StopOutcome {
                stopped: vec![],
                already_stopped: true,
                ambiguous: vec![],
            });
        }
        if mine.len() > 1 {
            // Two processes could be this session's. A coin flip here kills the wrong one and
            // takes an owner's in-flight turn with it, so nothing is signalled.
            return Err(EngineError::of(
                key.provider_id,
                ErrorKind::ProcessAmbiguous,
            ));
        }

        let target = mine[0];
        let Some(pid) = target.pid else {
            return Err(EngineError::of(
                key.provider_id,
                ErrorKind::ProcessAmbiguous,
            ));
        };
        // Who that pid *is*, taken from the same `ps` pass that proved it belongs to the session,
        // so exit can be verified against the process rather than against the number it happens
        // to hold. A row can only exist for a pid this pass saw alive, so a miss here means the
        // observation and the table disagree — refuse rather than signal an unproven number.
        let Some(identity) = table.alive(pid).map(ProcessIdentity::of) else {
            return Err(EngineError::of(
                key.provider_id,
                ErrorKind::ProcessAmbiguous,
            ));
        };

        // Related-but-unproven neighbours are reported, never stopped. The real case, measured
        // 2026-09-13: pid 52362 (`node …/codex resume`) parents pid 52363, and only the child
        // holds the writer lock. The wrapper normally exits with its child; if it does not, the
        // owner can see it here rather than wonder why a row lingers.
        let ambiguous = table
            .parent_of(pid)
            .filter(|p| p.pid > 1)
            .map(|p| AmbiguousMatch {
                pid: p.pid,
                why: note(format!(
                    "pid {} is the parent of pid {pid} but holds no proof of this session, \
                     so it was not stopped",
                    p.pid
                )),
            })
            .into_iter()
            .collect();

        let mut evidence = target.evidence.clone();
        evidence.push(note(format!("SIGTERM was sent to pid {pid}")));
        self.signaller
            .signal(pid, StopSignal::Term)
            .map_err(|f| probe_error(Some(key.provider_id), f, KILL))?;

        if self.wait_for_exit(&identity) {
            self.invalidate();
            let stopped = vec![StoppedProcess {
                pid,
                signal: StopSignal::Term,
                evidence,
            }];
            return Ok(StopOutcome {
                stopped,
                already_stopped: false,
                ambiguous,
            });
        }

        evidence.push(note(format!(
            "pid {pid} outlived the {} ms grace period, so SIGKILL was sent",
            self.stop_grace.as_millis()
        )));
        self.signaller
            .signal(pid, StopSignal::Kill)
            .map_err(|f| probe_error(Some(key.provider_id), f, KILL))?;
        if !self.wait_for_exit(&identity) {
            return Err(EngineError::of(
                key.provider_id,
                ErrorKind::ProcessStopFailed,
            ));
        }
        self.invalidate();
        let stopped = vec![StoppedProcess {
            pid,
            signal: StopSignal::Kill,
            evidence,
        }];
        Ok(StopOutcome {
            stopped,
            already_stopped: false,
            ambiguous,
        })
    }

    /// Poll `ps` until the proved process is gone or the grace period expires. Verification, not
    /// a guess: a successful `kill` only means the signal was delivered.
    ///
    /// **Gone means the process, not the number.** Returning `false` is the only thing that
    /// authorises SIGKILL, so it may be returned only while the target *itself* is still
    /// demonstrably there — see [`ProcessIdentity`] for why a bare pid match is not that.
    fn wait_for_exit(&self, target: &ProcessIdentity) -> bool {
        let deadline = std::time::Instant::now() + self.stop_grace;
        loop {
            match self.process.snapshot() {
                Ok(facts) if !facts.iter().any(|f| target.is_still(f)) => return true,
                // A probe failure is not evidence of exit. Keep waiting; the deadline ends it.
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(STOP_POLL);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// Live processes indexed for two questions: is this pid alive, and who is its parent.
struct ProcessTable {
    facts: Vec<ProcessFact>,
}

impl ProcessTable {
    fn new(facts: Vec<ProcessFact>) -> Self {
        Self { facts }
    }

    fn alive(&self, pid: u32) -> Option<&ProcessFact> {
        self.facts.iter().find(|f| f.pid == pid)
    }

    fn parent_of(&self, pid: u32) -> Option<&ProcessFact> {
        let ppid = self.alive(pid)?.ppid;
        self.alive(ppid)
    }
}

/// Who a pid *was* at the moment it was proved to belong to the session.
///
/// A pid is a number the kernel re-issues, and this Mac recycles them constantly: measured
/// 2026-09-13, 688 processes were live with `kern.maxproc` at 4000 and the highest live pid
/// already **99682** against macOS's 99999 ceiling. So a target that exits early in the two-second
/// grace period can have its number handed to an unrelated process before the next poll. Matching
/// on the number alone would then read as "still running" and escalate SIGKILL onto a stranger.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    /// `comm` whole rather than [`ProcessFact::name`]: two different binaries share a basename
    /// often enough (every `node`), and this comparison is not the one to be generous in.
    comm: String,
    ppid: u32,
}

impl ProcessIdentity {
    fn of(fact: &ProcessFact) -> Self {
        Self {
            pid: fact.pid,
            comm: fact.comm.clone(),
            ppid: fact.ppid,
        }
    }

    /// Is this fact still the process we proved? A mismatch on either half is read as the number
    /// having moved on, and that asymmetry is deliberate: calling a survivor gone costs one
    /// escalation we then do not send, while calling a stranger the target is an unrecoverable
    /// kill. The one benign case it gives up on is reparenting — measured 2026-09-13, pid 52362
    /// (`node …/codex resume`) parents the binary that holds the lock, and a wrapper that exits
    /// first moves its child's ppid to 1 — which reads here as gone, so SIGTERM stands and the
    /// owner sees the row again on the next poll rather than a process being killed twice over.
    fn is_still(&self, fact: &ProcessFact) -> bool {
        fact.pid == self.pid && fact.comm == self.comm && fact.ppid == self.ppid
    }
}

/// Bound one evidence sentence. Evidence is for the owner to check our reasoning, not a log.
fn note(sentence: String) -> String {
    tidy_title(&sentence, 200)
}

fn probe_error(provider: Option<ProviderId>, failure: ProbeFailure, program: &str) -> EngineError {
    match failure {
        ProbeFailure::Timeout => EngineError::new(provider, ErrorKind::Busy, ErrorDetail::None),
        _ => EngineError::new(
            provider,
            ErrorKind::Io,
            ErrorDetail::Path {
                path: program.to_string(),
            },
        ),
    }
}

/// One key, one row.
///
/// Duplicates are ordinary, not exotic: resuming a session in a second terminal — which Pigeon's
/// own Resume button does — leaves two live processes on one session id. Measured here today: pids
/// 3975 and 56461 both name `460e1a93`, one `busy` and one `idle`.
///
/// **The busiest process decides the state, not the most recent one.** Ordering by recency alone
/// meant a session whose second terminal had just gone idle reported "needs you" while its first
/// was actively working — the badge answering a question about a process rather than about the
/// session. `Running` therefore outranks `NeedsYou`, which outranks `Unknown`, and recency only
/// breaks a tie within one state. Evidence from every process is merged, so the detail pane still
/// shows what each one said and the owner can see why.
///
/// [`StatusService::stop_session`] still refuses this case as ambiguous. Counting once and killing
/// nothing are different questions: a badge may be approximate, a kill may not.
/// How loudly a state speaks for a session. Lower sorts first, so `Running` wins.
fn rank(state: LiveState) -> u8 {
    match state {
        LiveState::Running => 0,
        LiveState::Delegating => 1,
        LiveState::NeedsYou => 2,
        LiveState::Waiting => 3,
        LiveState::Unknown => 4,
    }
}

fn collapse(mut raw: Vec<LiveObservation>) -> Vec<LiveObservation> {
    raw.sort_by(|a, b| {
        a.key
            .cmp(&b.key)
            .then(rank(a.state).cmp(&rank(b.state)))
            .then(b.since_ms.cmp(&a.since_ms))
    });
    let mut out: Vec<LiveObservation> = Vec::with_capacity(raw.len());
    for obs in raw {
        match out.last_mut() {
            Some(last) if last.key == obs.key => {
                last.active_subagents = last.active_subagents.max(obs.active_subagents);
                last.evidence.extend(obs.evidence);
            }
            _ => out.push(obs),
        }
    }
    out
}

/// One `~/.claude/sessions/<pid>.json`.
///
/// `Err(field)` means the shape drifted; `Ok(None)` means the pid is not alive, which is the
/// normal state of a file a crashed CLI left behind.
fn claude_observation(
    path: &Path,
    value: &serde_json::Value,
    table: &ProcessTable,
    projects_root: &Path,
) -> Result<Option<LiveObservation>, String> {
    let Some(pid) = value.get("pid").and_then(|v| v.as_u64()) else {
        return Err("pid".into());
    };
    // `as u32` would truncate, and the stem cross-check below would then compare against the
    // truncated value rather than the declared one: a file named `1234.json` declaring
    // 2^32 + 1234 would agree with itself and attribute the row to whatever unrelated process
    // holds pid 1234 — which `stop_session` would then signal. Out of range is drift, stated.
    let Ok(pid) = u32::try_from(pid) else {
        return Err("pid".into());
    };
    let Some(sid) = value.get("sessionId").and_then(|v| v.as_str()) else {
        return Err("sessionId".into());
    };
    let Some(word) = value.get("status").and_then(|v| v.as_str()) else {
        return Err("status".into());
    };
    if sid.trim().is_empty() {
        return Err("sessionId".into());
    }

    // The filename stem is the pid too. The field is authoritative and the stem is the check: if
    // they disagree the file is not what we think it is, and guessing which one is right is
    // exactly the silent-wrong-number failure the invariants forbid.
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if stem.parse::<u32>() != Ok(pid) {
        return Err("pid".into());
    }

    // A crashed CLI leaves its file behind, so the file is never trusted on its own.
    let Some(fact) = table.alive(pid) else {
        return Ok(None);
    };

    let file = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("the status file");
    let mut evidence = vec![
        note(format!("{file} names session {sid}")),
        note(format!("pid {pid} is alive as \"{}\"", fact.name())),
        note(format!(
            "Claude Code publishes status \"{word}\" for pid {pid}"
        )),
    ];
    // The three owner cases (permission, question, interruption) are resolved by the adapter's
    // shared policy before the ordinary running/waiting/unknown turn call. Every engine reaches
    // this decision through the same `WaitPolicy`, so none of them can quietly skip a case.
    let tail = claude::claude_tail_facts(projects_root, sid);
    let active_subagents =
        claude::claude_subagent_facts(projects_root, sid).map_or(0, |facts| facts.active);
    let wait = claude::ClaudeWait::new(word, tail.as_ref());
    let (state, raw_word) = match wait.owner_wait() {
        Some(signal) => {
            if let Some(reason) = signal.evidence {
                evidence.push(note(reason));
            }
            (
                signal.case.state(),
                signal.raw_word.unwrap_or_else(|| word.to_string()),
            )
        }
        None => {
            let state = wait.turn_state();
            if state == LiveState::Unknown {
                evidence.push(note(format!(
                    "\"{word}\" is not a status word Pigeon recognises, so its state is unknown"
                )));
            }
            (state, word.to_string())
        }
    };
    if state == LiveState::Waiting && active_subagents > 0 {
        evidence.push(note(format!(
            "Claude Code is idle while {active_subagents} delegated subagent(s) are still active"
        )));
    }
    let state = if state == LiveState::Waiting && active_subagents > 0 {
        LiveState::Delegating
    } else {
        state
    };
    let since_ms = value
        .get("statusUpdatedAt")
        .or_else(|| value.get("updatedAt"))
        .and_then(|v| v.as_i64());

    Ok(Some(LiveObservation {
        key: SessionKey::new(ProviderId::ClaudeCode, sid),
        process: ProcessPresence::Present,
        state,
        since_ms,
        // The engine's own word, quoted back rather than translated away.
        raw_word: Some(raw_word),
        evidence,
        pid: Some(pid),
        console_id: None,
        active_subagents,
        observed_at_ms: now_ms(),
    }))
}

/// `…/thread-writer-locks/01a0963c-e907-7372-85be-d56b6669b13a.lock` → the thread id, **whole**.
/// Codex's ids are 36-char uuids and are passed to `codex resume` unchanged, so truncating one
/// here would produce a key that resumes nothing.
fn lock_thread_id(path: &Path) -> Option<String> {
    if path.extension().and_then(|e| e.to_str()) != Some("lock") {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    // `.coordination.lock` sits in the same directory and names no thread.
    if !crate::util::is_uuid(stem) {
        return None;
    }
    Some(stem.to_string())
}

/// What one scoped `lsof` pass found for a single Codex pid. Both halves come from the same call,
/// which is why they are grouped rather than probed twice.
#[derive(Default)]
struct CodexFiles {
    lock: Option<CodexLock>,
    rollout: Option<PathBuf>,
}

/// A held writer lock, and the thread id its filename carries.
struct CodexLock {
    thread: String,
    path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::LiveCounts;
    use std::sync::Mutex;

    /// A planted prompt. If this string ever reaches an evidence sentence, a private prompt has
    /// leaked, and the test that looks for it is the one that says so.
    const PROMPT: &str = "refactor the PRIVATEPROMPTCANARY billing module";

    // -- Fakes ---------------------------------------------------------------------------------

    #[derive(Default)]
    struct FakeProcesses {
        facts: Vec<ProcessFact>,
        fail: Option<ProbeFailure>,
    }

    impl FakeProcesses {
        /// Every fact carries a command line containing [`PROMPT`], which the real probe would
        /// never read. Nothing in the service may put it into evidence.
        fn alive(pids: &[(u32, u32, &str)]) -> Self {
            Self {
                facts: pids
                    .iter()
                    .map(|(pid, ppid, comm)| ProcessFact {
                        pid: *pid,
                        ppid: *ppid,
                        comm: (*comm).to_string(),
                        command: Some(format!("{comm} {PROMPT}")),
                    })
                    .collect(),
                fail: None,
            }
        }
    }

    impl ProcessProbe for FakeProcesses {
        fn snapshot(&self) -> Result<Vec<ProcessFact>, ProbeFailure> {
            match self.fail {
                Some(failure) => Err(failure),
                None => Ok(self.facts.clone()),
            }
        }
    }

    #[derive(Default)]
    struct FakeFiles {
        open: Vec<(u32, PathBuf)>,
        cwds: Vec<(u32, PathBuf)>,
    }

    impl OpenFileProbe for FakeFiles {
        fn open_files(&self, _program: &str) -> Result<Vec<(u32, PathBuf)>, ProbeFailure> {
            Ok(self.open.clone())
        }
        fn working_dirs(&self, _program: &str) -> Result<Vec<(u32, PathBuf)>, ProbeFailure> {
            Ok(self.cwds.clone())
        }
    }

    /// Records signals instead of sending them. A test that asserts "nothing was stopped" asserts
    /// against this, not against a comment.
    #[derive(Default)]
    struct Recorder {
        sent: Mutex<Vec<(u32, StopSignal)>>,
    }

    impl Recorder {
        fn sent(&self) -> Vec<(u32, StopSignal)> {
            self.sent.lock().expect("recorder").clone()
        }
    }

    impl Signaller for Recorder {
        fn signal(&self, pid: u32, signal: StopSignal) -> Result<(), ProbeFailure> {
            self.sent.lock().expect("recorder").push((pid, signal));
            Ok(())
        }
    }

    // -- Fixtures ------------------------------------------------------------------------------

    struct Harness {
        _dir: tempfile::TempDir,
        roots: StatusRoots,
        recorder: Arc<Recorder>,
    }

    impl Harness {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let roots = StatusRoots {
                claude_sessions: dir.path().join(".claude/sessions"),
                claude_projects: dir.path().join(".claude/projects"),
                codex_locks: dir.path().join(".codex/thread-writer-locks"),
                codex_sessions: dir.path().join(".codex/sessions"),
            };
            for root in [
                &roots.claude_sessions,
                &roots.claude_projects,
                &roots.codex_locks,
                &roots.codex_sessions,
            ] {
                std::fs::create_dir_all(root).expect("roots");
            }
            Self {
                _dir: dir,
                roots,
                recorder: Arc::new(Recorder::default()),
            }
        }

        /// Write one `~/.claude/sessions/<pid>.json` shaped exactly like the real ones measured on
        /// this Mac, differing only in the fields a test varies.
        fn claude_file(&self, pid: u32, sid: &str, status: &str) {
            let body = serde_json::json!({
                "pid": pid,
                "sessionId": sid,
                "cwd": "/Users/owner/Projects/feather",
                "startedAt": 1_789_333_950_694i64,
                "version": "2.1.270",
                "kind": "interactive",
                "entrypoint": "cli",
                "messagingSocketPath": format!("/tmp/cc-socks/{pid}.sock"),
                "name": "feather-f6",
                "status": status,
                "updatedAt": 1_789_334_207_814i64,
                "statusUpdatedAt": 1_789_334_207_814i64,
            });
            let path = self.roots.claude_sessions.join(format!("{pid}.json"));
            std::fs::write(path, serde_json::to_vec(&body).expect("json")).expect("write");
            // The real directory also holds a `<pid>.<hash>.key` sibling. Plant one so the test
            // proves it is skipped rather than parsed.
            let key = self.roots.claude_sessions.join(format!("{pid}.abc123.key"));
            std::fs::write(key, b"not json").expect("write");
        }

        fn claude_transcript(&self, sid: &str, records: &[&str]) {
            let project = self.roots.claude_projects.join("project");
            std::fs::create_dir_all(&project).expect("project");
            std::fs::write(
                project.join(format!("{sid}.jsonl")),
                records.join("\n") + "\n",
            )
            .expect("transcript");
        }

        /// The common case: some live Claude CLIs and no `lsof` results.
        fn with_pids(&self, pids: &[(u32, u32, &str)]) -> StatusService {
            self.service(FakeProcesses::alive(pids), FakeFiles::default())
        }

        fn service(&self, processes: FakeProcesses, files: FakeFiles) -> StatusService {
            StatusService::with_roots(self.roots.clone())
                .with_probes(
                    Arc::new(processes),
                    Arc::new(files),
                    Arc::clone(&self.recorder) as Arc<dyn Signaller>,
                )
                // Never the owner's real event file: a test that read it would pass or fail on
                // whether Codex happened to be asking for an approval on this machine.
                .with_codex_events(self.codex_events_path())
                .with_stop_grace(Duration::from_millis(0))
        }

        fn codex_events_path(&self) -> PathBuf {
            self._dir.path().join("codex-hook-events.jsonl")
        }

        /// Append hook events exactly as the installed Codex hook would.
        fn codex_hook_events(&self, records: &[&str]) {
            std::fs::write(self.codex_events_path(), records.join("\n") + "\n").expect("hooks");
        }
    }

    fn claude_key(sid: &str) -> SessionKey {
        SessionKey::new(ProviderId::ClaudeCode, sid)
    }

    const SID_A: &str = "460e1a93-2c29-4679-9ea0-a95f263ce79d";
    const SID_B: &str = "6858c8c4-71f5-4dde-907a-bcdd117984dc";

    fn counts(running: u32, needs_you: u32, finished: u32, unknown: u32) -> LiveCounts {
        LiveCounts {
            running,
            needs_you,
            finished,
            unknown,
        }
    }

    // -- 1..4: the Claude Code status file -----------------------------------------------------

    #[test]
    fn a_busy_claude_status_file_is_running_and_keeps_the_engines_own_word() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "busy");
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let report = service.refresh();
        assert_eq!(report.snapshot.live.len(), 1, "one live row");
        let obs = &report.snapshot.live[0];
        assert_eq!(obs.key, claude_key(SID_A));
        assert_eq!(obs.state, LiveState::Running);
        assert_eq!(obs.process, ProcessPresence::Present);
        assert_eq!(obs.raw_word.as_deref(), Some("busy"));
        assert_eq!(obs.pid, Some(4123));
        assert_eq!(obs.since_ms, Some(1_789_334_207_814));
        assert_eq!(report.snapshot.counts(), counts(1, 0, 0, 0));
    }

    #[test]
    fn a_stale_busy_word_does_not_make_a_completed_claude_turn_running() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "busy");
        h.claude_transcript(
            SID_A,
            &[r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#],
        );
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let report = service.refresh();
        let obs = &report.snapshot.live[0];
        assert_eq!(obs.state, LiveState::Waiting);
        assert_eq!(report.snapshot.counts(), counts(0, 0, 1, 0));
    }

    #[test]
    fn an_interrupted_claude_turn_is_waiting_even_if_the_status_file_still_says_busy() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "busy");
        h.claude_transcript(
            SID_A,
            &[
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use"}]}}"#,
                r#"{"type":"user","interruptedMessageId":"msg_123","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#,
            ],
        );
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let report = service.refresh();
        let obs = &report.snapshot.live[0];
        assert_eq!(obs.state, LiveState::Waiting);
        assert_eq!(report.snapshot.counts(), counts(0, 0, 1, 0));
    }

    #[test]
    fn claudes_idle_word_wins_when_an_interruption_leaves_only_a_user_record() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "idle");
        h.claude_transcript(
            SID_A,
            &[r#"{"type":"user","origin":{"kind":"human"},"message":{"role":"user","content":"stop here"}}"#],
        );
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let report = service.refresh();
        let obs = &report.snapshot.live[0];
        assert_eq!(obs.state, LiveState::Waiting);
        assert_eq!(report.snapshot.counts(), counts(0, 0, 1, 0));
    }

    #[test]
    fn idle_waits_and_needs_input_needs_the_owner_and_each_keeps_its_own_word() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "idle");
        h.claude_file(4124, SID_B, "needs_input");
        let service = h.with_pids(&[(4123, 4100, "claude"), (4124, 4100, "claude")]);

        let report = service.refresh();
        let mut words: Vec<(LiveState, String)> = report
            .snapshot
            .live
            .iter()
            .map(|o| (o.state, o.raw_word.clone().expect("raw word")))
            .collect();
        words.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(
            words,
            vec![
                (LiveState::Waiting, "idle".to_string()),
                (LiveState::NeedsYou, "needs_input".to_string()),
            ],
            "idle is ordinary waiting; needs_input is the engine parked on the owner"
        );
        // Neither is active work, but they are not the same thing: one needs the owner and one
        // does not. `needs_input` counts as needs-you, not as running and not as unknown.
        assert_eq!(report.snapshot.counts(), counts(0, 1, 1, 0));
    }

    /// **Claude sitting on `AskUserQuestion` must not read as running.**
    ///
    /// The engine publishes `needs_input`, but a question leaves a trailing `tool_use`, so the
    /// transcript tail says `Running`. Before this, that tail overrode the published word and the
    /// owner saw an agent working when it was in fact blocked on them.
    #[test]
    fn needs_input_outranks_a_transcript_that_still_looks_open() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "needs_input");
        h.claude_transcript(
            SID_A,
            &[r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_q","name":"AskUserQuestion","input":{}}]}}"#],
        );
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let report = service.refresh();
        let obs = &report.snapshot.live[0];
        assert_eq!(obs.state, LiveState::NeedsYou);
        assert_eq!(obs.raw_word.as_deref(), Some("needs_input"));
        assert_eq!(report.snapshot.counts(), counts(0, 1, 0, 0));
    }

    /// **`frds.md` row 5 / T-19.4**: an `idle` file whose tail ends on an unanswered
    /// `AskUserQuestion` is the engine parked on the owner, not finished.
    ///
    /// This is the case `idle` alone cannot express. Claude reaches `idle` while the question is
    /// still on screen, and the trailing `tool_use` is what distinguishes it from the interruption
    /// case that must stay waiting.
    #[test]
    fn an_idle_file_with_an_unanswered_question_in_the_tail_needs_the_owner() {
        for tool in ["AskUserQuestion", "ExitPlanMode"] {
            let h = Harness::new();
            h.claude_file(4123, SID_A, "idle");
            h.claude_transcript(
                SID_A,
                &[&format!(
                    r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"toolu_q","name":"{tool}","input":{{}}}}]}}}}"#
                )],
            );
            let service = h.with_pids(&[(4123, 4100, "claude")]);

            let report = service.refresh();
            assert_eq!(
                report.snapshot.live[0].state,
                LiveState::NeedsYou,
                "{tool} left outstanding must read as needs-you"
            );
            assert_eq!(report.snapshot.counts(), counts(0, 1, 0, 0), "{tool}");
        }
    }

    /// The same shape, but the question has been ANSWERED: the tool result is the newest record.
    /// Reading the earlier `tool_use` as outstanding would leave a session stuck on needs-you
    /// after the owner had already replied.
    #[test]
    fn an_answered_question_does_not_leave_the_session_needing_the_owner() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "idle");
        h.claude_transcript(
            SID_A,
            &[
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_q","name":"AskUserQuestion","input":{}}]}}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_q","content":"Add the test"}]}}"#,
            ],
        );
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let report = service.refresh();
        assert_eq!(report.snapshot.live[0].state, LiveState::Waiting);
    }

    /// `idle` with an ordinary open-looking tail still wins: that is the interruption case, and
    /// the transcript must not reopen it as running.
    #[test]
    fn idle_still_wins_over_an_ordinary_open_tool_call() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "idle");
        h.claude_transcript(
            SID_A,
            &[r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_r","name":"Read","input":{}}]}}"#],
        );
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let report = service.refresh();
        assert_eq!(report.snapshot.live[0].state, LiveState::Waiting);
    }

    /// The two words the contract lists that the mapper used to miss entirely.
    #[test]
    fn the_contracts_other_status_words_map_as_written() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "running");
        h.claude_file(4124, SID_B, "waiting");
        let service = h.with_pids(&[(4123, 4100, "claude"), (4124, 4100, "claude")]);

        let report = service.refresh();
        let mut by_word: Vec<(String, LiveState)> = report
            .snapshot
            .live
            .iter()
            .map(|o| (o.raw_word.clone().expect("raw word"), o.state))
            .collect();
        by_word.sort();
        assert_eq!(
            by_word,
            vec![
                ("running".to_string(), LiveState::Running),
                ("waiting".to_string(), LiveState::NeedsYou),
            ]
        );
    }

    /// The other direction must survive: a stale `busy` is still closed by a transcript that
    /// plainly finished the turn, or a completed session would read as running forever.
    #[test]
    fn a_stale_busy_word_is_still_closed_by_a_finished_transcript() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "busy");
        h.claude_transcript(
            SID_A,
            &[r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#],
        );
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let report = service.refresh();
        assert_eq!(report.snapshot.live[0].state, LiveState::Waiting);
    }

    #[test]
    fn an_unrecognised_status_word_is_unknown_and_the_word_itself_survives() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "compacting");
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let report = service.refresh();
        let obs = &report.snapshot.live[0];
        assert_eq!(
            obs.state,
            LiveState::Unknown,
            "an unknown word is not guessed into a state"
        );
        assert_eq!(
            obs.raw_word.as_deref(),
            Some("compacting"),
            "the word is kept, not discarded"
        );
        assert!(
            obs.evidence.iter().any(|e| e.contains("compacting")),
            "the evidence names the word we did not recognise: {:?}",
            obs.evidence
        );
        assert_eq!(report.snapshot.counts(), counts(0, 0, 0, 1));
    }

    #[test]
    fn a_status_file_whose_pid_is_not_alive_produces_no_observation_at_all() {
        let h = Harness::new();
        // A crashed CLI leaves its file behind. `busy` in a file is not a running process.
        h.claude_file(4123, SID_A, "busy");
        let service = h.with_pids(&[(9999, 1, "claude")]);

        let report = service.refresh();
        assert!(
            report.snapshot.live.is_empty(),
            "a stale file is not a live session"
        );
        // Zero, not `finished: 1`. `counts()` folds over `snapshot.live`, which the assertion
        // above proves is empty — so no bucket can be non-zero here. A stale file from a
        // crashed CLI is also not evidence the session *finished*: invariant 6 keeps absence
        // and zero apart, and this session has no status at all.
        assert_eq!(report.snapshot.counts(), counts(0, 0, 0, 0));
        assert!(
            report.problems.is_empty(),
            "a dead pid is normal, not a failure to report"
        );
    }

    // -- 5: Codex attribution ------------------------------------------------------------------

    #[test]
    fn a_codex_writer_lock_filename_yields_the_whole_thread_id() {
        let thread = "01a0963c-e907-7372-85be-d56b6669b13a";
        let path = PathBuf::from(format!(
            "/Users/owner/.codex/thread-writer-locks/{thread}.lock"
        ));
        let parsed = lock_thread_id(&path).expect("a uuid stem is a thread id");
        assert_eq!(parsed, thread);
        assert_eq!(
            parsed.len(),
            36,
            "never truncated — `codex resume` takes the id whole"
        );

        // The coordination lock shares the directory and names no thread.
        let other = PathBuf::from("/Users/owner/.codex/thread-writer-locks/.coordination.lock");
        assert_eq!(lock_thread_id(&other), None);

        let h = Harness::new();
        let lock = h.roots.codex_locks.join(format!("{thread}.lock"));
        std::fs::write(&lock, b"").expect("lock");
        let rollout = h
            .roots
            .codex_sessions
            .join("rollout-2026-09-12T10-29-29.jsonl");
        std::fs::write(
            &rollout,
            b"{\"timestamp\":\"2026-09-13T21:04:17.468Z\",\"type\":\"event_msg\",\
              \"payload\":{\"type\":\"task_complete\"}}\n",
        )
        .expect("rollout");
        let open = vec![(52363, lock), (52363, rollout)];
        let files = FakeFiles {
            open,
            ..Default::default()
        };
        let codex_bin = "/Users/owner/node_modules/@openai/codex-darwin-arm64/bin/codex";
        let service = h.service(FakeProcesses::alive(&[(52363, 52362, codex_bin)]), files);

        let report = service.refresh();
        assert_eq!(report.snapshot.live.len(), 1);
        let obs = &report.snapshot.live[0];
        assert_eq!(obs.key, SessionKey::new(ProviderId::Codex, thread));
        assert_eq!(obs.key.sid.len(), 36);
        assert_eq!(
            obs.state,
            LiveState::Waiting,
            "a closed turn means Codex is at its prompt"
        );
        assert_eq!(obs.raw_word.as_deref(), Some("task_complete"));
        assert_eq!(obs.since_ms, Some(1_789_333_457_468));
    }

    /// A Codex holding an open turn and a `PermissionRequest` hook is **waiting on the owner**.
    ///
    /// The rollout alone cannot say this: a Codex stopped on an approval prompt still has its last
    /// turn record as `task_started`, so the tail reports Running. This is the whole reason the
    /// hook is read, and it is the bug the owner reported — "waiting on a question shows as
    /// running" — in its approval form.
    #[test]
    fn a_codex_permission_request_hook_overrides_an_open_rollout_turn() {
        let h = Harness::new();
        let thread = "01a0963c-e907-7372-85be-d56b6669b13a";
        let lock = h.roots.codex_locks.join(format!("{thread}.lock"));
        std::fs::write(&lock, b"").expect("lock");
        let rollout = h.roots.codex_sessions.join("rollout.jsonl");
        std::fs::write(
            &rollout,
            b"{\"timestamp\":\"2026-09-13T21:00:00.000Z\",\
              \"payload\":{\"type\":\"task_started\"}}\n",
        )
        .expect("rollout");
        h.codex_hook_events(&[&format!(
            "{{\"session_id\":\"{thread}\",\"hook_event_name\":\"PermissionRequest\"}}"
        )]);

        let files = FakeFiles {
            open: vec![(52363, lock), (52363, rollout)],
            ..Default::default()
        };
        let codex_bin = "/Users/owner/node_modules/@openai/codex-darwin-arm64/bin/codex";
        let service = h.service(FakeProcesses::alive(&[(52363, 52362, codex_bin)]), files);

        let report = service.refresh();
        let obs = &report.snapshot.live[0];
        assert_eq!(obs.key, SessionKey::new(ProviderId::Codex, thread));
        assert_eq!(obs.state, LiveState::NeedsYou);
        assert_eq!(obs.raw_word.as_deref(), Some("PermissionRequest"));
        assert_eq!(report.snapshot.counts(), counts(0, 1, 0, 0));
        assert!(
            obs.evidence
                .iter()
                .any(|line| line.contains("PermissionRequest")),
            "the evidence names the hook that decided it: {:?}",
            obs.evidence
        );
    }

    /// The hook may NOT overrule a turn the rollout has already closed.
    ///
    /// A `PermissionRequest` line stays in the append-only file for the rest of time. If it were
    /// allowed to override any state, a session Codex finished an hour ago would come back as
    /// "needs you" forever.
    #[test]
    fn a_stale_permission_request_cannot_resurrect_a_finished_codex_turn() {
        let h = Harness::new();
        let thread = "01a0963c-e907-7372-85be-d56b6669b13a";
        let lock = h.roots.codex_locks.join(format!("{thread}.lock"));
        std::fs::write(&lock, b"").expect("lock");
        let rollout = h.roots.codex_sessions.join("rollout.jsonl");
        std::fs::write(
            &rollout,
            b"{\"timestamp\":\"2026-09-13T21:04:17.468Z\",\
              \"payload\":{\"type\":\"task_complete\"}}\n",
        )
        .expect("rollout");
        h.codex_hook_events(&[&format!(
            "{{\"session_id\":\"{thread}\",\"hook_event_name\":\"PermissionRequest\"}}"
        )]);

        let files = FakeFiles {
            open: vec![(52363, lock), (52363, rollout)],
            ..Default::default()
        };
        let codex_bin = "/Users/owner/node_modules/@openai/codex-darwin-arm64/bin/codex";
        let service = h.service(FakeProcesses::alive(&[(52363, 52362, codex_bin)]), files);

        let report = service.refresh();
        assert_eq!(report.snapshot.live[0].state, LiveState::Waiting);
        assert_eq!(report.snapshot.counts(), counts(0, 0, 1, 0));
    }

    /// An event that is not an approval changes nothing: the rollout tail remains the authority on
    /// running versus idle.
    #[test]
    fn a_non_approval_hook_event_leaves_the_rollout_reading_alone() {
        let h = Harness::new();
        let thread = "01a0963c-e907-7372-85be-d56b6669b13a";
        let lock = h.roots.codex_locks.join(format!("{thread}.lock"));
        std::fs::write(&lock, b"").expect("lock");
        let rollout = h.roots.codex_sessions.join("rollout.jsonl");
        std::fs::write(
            &rollout,
            b"{\"timestamp\":\"2026-09-13T21:00:00.000Z\",\
              \"payload\":{\"type\":\"task_started\"}}\n",
        )
        .expect("rollout");
        h.codex_hook_events(&[&format!(
            "{{\"session_id\":\"{thread}\",\"hook_event_name\":\"PreToolUse\"}}"
        )]);

        let files = FakeFiles {
            open: vec![(52363, lock), (52363, rollout)],
            ..Default::default()
        };
        let codex_bin = "/Users/owner/node_modules/@openai/codex-darwin-arm64/bin/codex";
        let service = h.service(FakeProcesses::alive(&[(52363, 52362, codex_bin)]), files);

        let report = service.refresh();
        assert_eq!(report.snapshot.live[0].state, LiveState::Running);
        assert_eq!(
            report.snapshot.live[0].raw_word.as_deref(),
            Some("task_started")
        );
    }

    // -- 6: absence ----------------------------------------------------------------------------

    #[test]
    fn a_session_with_no_live_process_is_absent_from_the_snapshot_and_counted_nowhere() {
        let h = Harness::new();
        // Two sessions exist on disk; only one has a live process.
        h.claude_file(4123, SID_A, "busy");
        h.claude_file(4124, SID_B, "idle");
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let report = service.refresh();
        let keys: Vec<SessionKey> = report.snapshot.live.iter().map(|o| o.key.clone()).collect();
        assert_eq!(keys, vec![claude_key(SID_A)]);
        assert!(!keys.contains(&claude_key(SID_B)), "no process, no row");
        // The closed one contributes to no count — not even Unknown.
        assert_eq!(report.snapshot.counts(), counts(1, 0, 0, 0));
    }

    // -- 7, 8: stop_session --------------------------------------------------------------------

    #[test]
    fn two_candidate_processes_for_one_session_refuse_to_stop_and_signal_nothing() {
        let h = Harness::new();
        // The real shape of this: `claude --resume <sid>` opened twice, so two live CLIs both
        // publish the same `sessionId`. Neither can be proven to be *the* one.
        h.claude_file(4123, SID_A, "busy");
        h.claude_file(4124, SID_A, "idle");
        let service = h.with_pids(&[(4123, 4100, "claude"), (4124, 4100, "claude")]);

        let err = service
            .stop_session(&claude_key(SID_A))
            .expect_err("ambiguous");
        assert_eq!(err.kind, ErrorKind::ProcessAmbiguous);
        assert_eq!(err.provider, Some(ProviderId::ClaudeCode));
        assert!(
            h.recorder.sent().is_empty(),
            "nothing was signalled: {:?}",
            h.recorder.sent()
        );

        // The badge still counts once, because two processes are still one session to look at.
        assert_eq!(service.refresh().snapshot.counts(), counts(1, 0, 0, 0));
    }

    #[test]
    fn stopping_a_session_with_no_matching_process_is_already_stopped_not_an_error() {
        let h = Harness::new();
        h.claude_file(4123, SID_B, "busy");
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let outcome = service
            .stop_session(&claude_key(SID_A))
            .expect("idempotent, not an error");
        assert!(outcome.already_stopped);
        assert!(outcome.stopped.is_empty());
        assert!(outcome.ambiguous.is_empty());
        assert!(h.recorder.sent().is_empty(), "nothing was signalled");
    }

    #[test]
    fn a_proven_match_is_sigtermed_and_its_unproven_parent_is_reported_untouched() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "busy");
        // The process is gone by the time `wait_for_exit` looks, so SIGTERM alone succeeds.
        struct Vanishing {
            first: Mutex<bool>,
        }
        impl ProcessProbe for Vanishing {
            fn snapshot(&self) -> Result<Vec<ProcessFact>, ProbeFailure> {
                let mut first = self.first.lock().expect("lock");
                if *first {
                    *first = false;
                    return Ok(
                        FakeProcesses::alive(&[(4123, 4100, "claude"), (4100, 1, "zsh")]).facts,
                    );
                }
                Ok(vec![])
            }
        }
        let service = StatusService::with_roots(h.roots.clone())
            .with_probes(
                Arc::new(Vanishing {
                    first: Mutex::new(true),
                }),
                Arc::new(FakeFiles::default()),
                Arc::clone(&h.recorder) as Arc<dyn Signaller>,
            )
            .with_stop_grace(Duration::from_millis(0));

        let outcome = service.stop_session(&claude_key(SID_A)).expect("stops");
        assert!(!outcome.already_stopped);
        assert_eq!(outcome.stopped.len(), 1);
        assert_eq!(outcome.stopped[0].pid, 4123);
        assert_eq!(outcome.stopped[0].signal, StopSignal::Term);
        assert_eq!(
            h.recorder.sent(),
            vec![(4123, StopSignal::Term)],
            "SIGTERM first, and only"
        );
        assert_eq!(
            outcome.ambiguous.len(),
            1,
            "the shell parent is reported, not stopped"
        );
        assert_eq!(outcome.ambiguous[0].pid, 4100);
        assert!(!h.recorder.sent().iter().any(|(pid, _)| *pid == 4100));
    }

    #[test]
    fn a_pid_recycled_inside_the_grace_period_is_not_escalated_to_sigkill() {
        // The sequence this defends against: the CLI exits a few ms after SIGTERM and the kernel
        // hands its number straight to something else before the next poll. Not theoretical on
        // this Mac — measured 2026-09-13, the highest live pid was 99682 of a 99999 ceiling with
        // only 688 processes live, so the numbers wrap constantly.
        struct Recycled {
            polls: Mutex<u32>,
            replacement: (u32, u32, &'static str),
        }
        impl ProcessProbe for Recycled {
            fn snapshot(&self) -> Result<Vec<ProcessFact>, ProbeFailure> {
                let mut polls = self.polls.lock().expect("lock");
                *polls += 1;
                if *polls == 1 {
                    // The pass that proves ownership sees the real target.
                    return Ok(FakeProcesses::alive(&[(4123, 4100, "claude")]).facts);
                }
                // Every later poll sees the same number held by someone else.
                Ok(FakeProcesses::alive(&[self.replacement]).facts)
            }
        }

        for (replacement, who) in [
            (
                (4123, 4100, "vim"),
                "a different binary under the same parent",
            ),
            (
                (4123, 77, "claude"),
                "a second claude CLI under a different parent",
            ),
        ] {
            let h = Harness::new();
            h.claude_file(4123, SID_A, "busy");
            let service = StatusService::with_roots(h.roots.clone())
                .with_probes(
                    Arc::new(Recycled {
                        polls: Mutex::new(0),
                        replacement,
                    }),
                    Arc::new(FakeFiles::default()),
                    Arc::clone(&h.recorder) as Arc<dyn Signaller>,
                )
                .with_stop_grace(Duration::from_millis(0));

            let outcome = service.stop_session(&claude_key(SID_A)).expect("stops");
            assert_eq!(
                h.recorder.sent(),
                vec![(4123, StopSignal::Term)],
                "SIGKILL would have landed on {who}, which we never proved anything about"
            );
            assert_eq!(outcome.stopped.len(), 1, "{who}");
            assert_eq!(outcome.stopped[0].signal, StopSignal::Term, "{who}");
            assert!(
                !outcome.stopped[0]
                    .evidence
                    .iter()
                    .any(|e| e.contains("SIGKILL")),
                "no evidence may record a SIGKILL that must not be sent: {who}"
            );
        }

        // The control, so the guard above is a discrimination and not a blanket refusal to
        // escalate: an unchanged identity is still the target, and it is still escalated.
        let h = Harness::new();
        h.claude_file(4123, SID_A, "busy");
        let service = h.with_pids(&[(4123, 4100, "claude")]);
        let err = service
            .stop_session(&claude_key(SID_A))
            .expect_err("it never exits");
        assert_eq!(err.kind, ErrorKind::ProcessStopFailed);
        assert_eq!(
            h.recorder.sent(),
            vec![(4123, StopSignal::Term), (4123, StopSignal::Kill)]
        );
    }

    #[test]
    fn the_signaller_refuses_pid_0_and_pid_1_whatever_its_caller_asks_for() {
        // `/bin/kill -TERM 0` signals the caller's own process group; measured on this Mac
        // 2026-09-13, `/bin/kill -0 0` exits 0 from this process. Pid 1 is launchd. No caller
        // in this file can reach either number today, which is why the guard belongs in the
        // component rather than in the callers.
        for pid in [0, 1] {
            for signal in [StopSignal::Term, StopSignal::Kill] {
                assert_eq!(
                    PosixSignaller.signal(pid, signal),
                    Err(ProbeFailure::Spawn),
                    "pid {pid} must never reach /bin/kill"
                );
            }
        }
        // The other half — that a real pid still goes through — is asserted everywhere the
        // `Recorder` seam records a signal, and deliberately not by sending one from a test.
    }

    // -- 9: the privacy canary -----------------------------------------------------------------

    #[test]
    fn no_evidence_string_anywhere_ever_contains_a_command_line() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "busy");
        h.claude_file(4124, SID_B, "not_a_word_we_know");
        let thread = "01a0963c-e907-7372-85be-d56b6669b13a";
        let lock = h.roots.codex_locks.join(format!("{thread}.lock"));
        std::fs::write(&lock, b"").expect("lock");
        let files = FakeFiles {
            open: vec![(52363, lock)],
            ..Default::default()
        };
        // Every fake fact's `command` holds PROMPT; so does the codex binary's own comm path.
        let processes = FakeProcesses::alive(&[
            (4123, 4100, "claude"),
            (4124, 4100, "claude"),
            (52363, 52362, "/opt/codex/bin/codex"),
        ]);
        let service = h.service(processes, files);

        let report = service.refresh();
        assert_eq!(
            report.snapshot.live.len(),
            3,
            "all three rows were produced"
        );
        let mut checked = 0;
        for obs in &report.snapshot.live {
            assert!(
                !obs.evidence.is_empty(),
                "an observation without evidence is not a product"
            );
            for line in &obs.evidence {
                assert!(
                    !line.contains("PRIVATEPROMPTCANARY"),
                    "a command line reached evidence: {line}"
                );
                checked += 1;
            }
        }
        assert!(
            checked >= 6,
            "the canary was actually exercised over {checked} sentences"
        );

        // And through stop, which builds evidence of its own.
        let outcome = service.stop_session(&claude_key(SID_A));
        if let Ok(outcome) = outcome {
            for stopped in &outcome.stopped {
                for line in &stopped.evidence {
                    assert!(
                        !line.contains("PRIVATEPROMPTCANARY"),
                        "leaked on the stop path"
                    );
                }
            }
            for ambiguous in &outcome.ambiguous {
                assert!(
                    !ambiguous.why.contains("PRIVATEPROMPTCANARY"),
                    "leaked in an ambiguity"
                );
            }
        }
    }

    // -- 10: a missing root is stated, never an empty success ----------------------------------

    #[test]
    fn a_status_root_that_does_not_exist_is_a_stated_absence_not_an_empty_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        let roots = StatusRoots {
            claude_sessions: dir.path().join("nope/claude"),
            claude_projects: dir.path().join("nope/projects"),
            codex_locks: dir.path().join("nope/codex-locks"),
            codex_sessions: dir.path().join("nope/codex-sessions"),
        };
        let service = StatusService::with_roots(roots).with_probes(
            Arc::new(FakeProcesses::alive(&[(4123, 4100, "claude")])),
            Arc::new(FakeFiles::default()),
            Arc::new(Recorder::default()),
        );

        let report = service.refresh();
        assert!(report.snapshot.live.is_empty());
        // The dangerous outcome is an empty-but-successful snapshot, which reads as "nothing is
        // running". Both roots must say so out loud instead.
        assert_eq!(
            report.problems.len(),
            2,
            "one per missing root: {:?}",
            report.problems
        );
        for problem in &report.problems {
            assert_eq!(problem.kind, ErrorKind::RootMissing);
            assert!(
                matches!(problem.detail, ErrorDetail::Path { .. }),
                "the path is the message"
            );
        }
        let providers: Vec<Option<ProviderId>> =
            report.problems.iter().map(|p| p.provider).collect();
        assert!(providers.contains(&Some(ProviderId::ClaudeCode)));
        assert!(providers.contains(&Some(ProviderId::Codex)));

        // And a stop against a root we cannot read is an error, never "already stopped" — that
        // would claim we looked when we did not.
        let err = service
            .stop_session(&claude_key(SID_A))
            .expect_err("cannot claim absence");
        assert_eq!(err.kind, ErrorKind::RootMissing);
    }

    // -- OpenCode: the documented seam ---------------------------------------------------------

    #[test]
    fn opencode_without_an_attributor_says_unsupported_rather_than_inventing_a_key() {
        let h = Harness::new();
        let files = FakeFiles {
            cwds: vec![
                (18619, PathBuf::from("/Users/owner/Projects/a")),
                (38692, PathBuf::from("/Users/owner/Projects/b")),
            ],
            ..Default::default()
        };
        let processes =
            FakeProcesses::alive(&[(18619, 18280, "opencode"), (38692, 38586, "opencode")]);
        let service = h.service(processes, files);

        let report = service.refresh();
        assert!(
            report.snapshot.live.is_empty(),
            "no key, no row — never a keyless Unknown"
        );
        let problem = report
            .problems
            .iter()
            .find(|p| p.provider == Some(ProviderId::OpenCode))
            .expect("the absence is stated");
        assert_eq!(problem.kind, ErrorKind::Unsupported);
    }

    #[test]
    fn opencode_with_an_attributor_produces_rows_carrying_both_halves_of_the_evidence() {
        struct Fake;
        impl OpenCodeAttributor for Fake {
            fn attribute(
                &self,
                processes: &[OpenCodeProcess],
            ) -> Result<Vec<OpenCodeAttribution>, EngineError> {
                Ok(processes
                    .iter()
                    .map(|p| OpenCodeAttribution {
                        pid: Some(p.pid),
                        sid: format!("ses_{}", p.pid),
                        state: LiveState::NeedsYou,
                        since_ms: Some(1_789_334_207_814),
                        raw_word: Some("permission".into()),
                        evidence: vec![format!("a pending permission row names pid {}", p.pid)],
                        active_subagents: 0,
                    })
                    .collect())
            }
        }
        let h = Harness::new();
        let files = FakeFiles {
            cwds: vec![(18619, PathBuf::from("/Users/owner/Projects/a"))],
            ..Default::default()
        };
        let service = h
            .service(FakeProcesses::alive(&[(18619, 18280, "opencode")]), files)
            .with_attributor(Arc::new(Fake));

        let report = service.refresh();
        assert_eq!(report.snapshot.live.len(), 1);
        let obs = &report.snapshot.live[0];
        assert_eq!(obs.key, SessionKey::new(ProviderId::OpenCode, "ses_18619"));
        assert_eq!(obs.state, LiveState::NeedsYou);
        assert!(
            obs.evidence.iter().any(|e| e.contains("is alive as")),
            "host half"
        );
        assert!(
            obs.evidence.iter().any(|e| e.contains("permission row")),
            "adapter half"
        );
    }

    #[test]
    fn an_attribution_naming_a_pid_that_was_never_proved_alive_produces_no_row_to_stop() {
        /// Names the one real process correctly and invents a second. The seam's contract is
        /// what the next implementer builds against, so it has to refuse this now.
        struct Stray;
        impl OpenCodeAttributor for Stray {
            fn attribute(
                &self,
                processes: &[OpenCodeProcess],
            ) -> Result<Vec<OpenCodeAttribution>, EngineError> {
                let attribution = |pid: u32, sid: &str| OpenCodeAttribution {
                    pid: Some(pid),
                    sid: sid.to_string(),
                    state: LiveState::Running,
                    since_ms: None,
                    raw_word: None,
                    evidence: vec![],
                    active_subagents: 0,
                };
                let mut out: Vec<OpenCodeAttribution> = processes
                    .iter()
                    .map(|p| attribution(p.pid, &format!("ses_{}", p.pid)))
                    .collect();
                out.push(attribution(4242, "ses_stray"));
                Ok(out)
            }
        }
        let h = Harness::new();
        let files = FakeFiles {
            cwds: vec![(18619, PathBuf::from("/Users/owner/Projects/a"))],
            ..Default::default()
        };
        let service = h
            .service(FakeProcesses::alive(&[(18619, 18280, "opencode")]), files)
            .with_attributor(Arc::new(Stray));

        let report = service.refresh();
        let keys: Vec<String> = report.snapshot.live.iter().map(|o| o.key.id()).collect();
        assert_eq!(
            report.snapshot.live.len(),
            1,
            "only the proved process may become a row: {keys:?}"
        );
        assert_eq!(report.snapshot.live[0].pid, Some(18619));
        assert!(
            !keys.iter().any(|k| k.contains("ses_stray")),
            "a row with no host evidence is a stoppable pid nobody proved: {keys:?}"
        );
        let problem = report
            .problems
            .iter()
            .find(|p| p.provider == Some(ProviderId::OpenCode))
            .expect("the breach of the seam contract is stated");
        assert_eq!(problem.kind, ErrorKind::UnknownShape);

        // And the stray pid cannot be reached through stop either.
        let err = service
            .stop_session(&SessionKey::new(ProviderId::OpenCode, "ses_stray"))
            .expect_err("no row, and a stated problem, so not `already_stopped`");
        assert_eq!(err.kind, ErrorKind::UnknownShape);
        assert!(h.recorder.sent().is_empty(), "nothing was signalled");
    }

    // -- Unit coverage for the parsers ---------------------------------------------------------

    #[test]
    fn ps_output_parses_into_pid_parent_and_a_basename() {
        let raw = concat!(
            "66906 61986 claude\n",
            " 3975  3870 claude\n",
            "52363 52362 /Users/k/node_modules/@openai/codex-darwin-arm64/bin/codex\n",
            "garbage line\n",
        );
        let facts = parse_ps(raw);
        assert_eq!(
            facts.len(),
            3,
            "the unparseable line is skipped, not guessed at"
        );
        assert_eq!(
            facts[1],
            ProcessFact {
                pid: 3975,
                ppid: 3870,
                comm: "claude".into(),
                command: None,
            }
        );
        assert_eq!(
            facts[2].name(),
            "codex",
            "only the basename may reach evidence"
        );
        assert!(
            facts.iter().all(|f| f.command.is_none()),
            "the real shape never carries argv"
        );
    }

    #[test]
    fn lsof_field_output_parses_and_ignores_tags_it_does_not_understand() {
        let raw = "p18619\nfcwd\nn/Users/k/Projects/a\np38692\nftxt\nnrelative-not-absolute\n\
                   fcwd\nn/Users/k/Projects/b\n";
        assert_eq!(
            parse_lsof(raw),
            vec![
                (18619, PathBuf::from("/Users/k/Projects/a")),
                (38692, PathBuf::from("/Users/k/Projects/b")),
            ]
        );
    }

    #[test]
    fn a_drifted_status_file_is_reported_loudly_and_the_readable_rows_still_travel() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "busy");
        // A file with no `sessionId` — the exact drift that would otherwise mint a keyless row.
        std::fs::write(
            h.roots.claude_sessions.join("4124.json"),
            br#"{"pid":4124,"status":"busy"}"#,
        )
        .expect("write");
        let service = h.with_pids(&[(4123, 4100, "claude"), (4124, 4100, "claude")]);

        let report = service.refresh();
        assert_eq!(
            report.snapshot.live.len(),
            1,
            "the good row survives the bad one"
        );
        let problem = report.problems.first().expect("the drift is stated");
        assert_eq!(problem.kind, ErrorKind::UnknownShape);
        assert!(matches!(problem.detail, ErrorDetail::Fields { .. }));
    }

    #[test]
    fn a_pid_field_too_large_for_u32_is_drift_and_is_never_truncated_onto_a_live_process() {
        let h = Harness::new();
        // 2^32 + 1234, in a file named `1234.json`. Truncating to u32 yields exactly 1234, so the
        // stem cross-check would agree with itself and the row would name a process that has
        // nothing to do with this session — one `stop_session` would then signal.
        std::fs::write(
            h.roots.claude_sessions.join("1234.json"),
            format!(r#"{{"pid":4294968530,"sessionId":"{SID_A}","status":"busy"}}"#),
        )
        .expect("write");
        // pid 1234 is alive on this fake host, and it is not Claude Code.
        let service = h.with_pids(&[(1234, 1, "loginwindow")]);

        let report = service.refresh();
        assert!(
            report.snapshot.live.is_empty(),
            "an out-of-range pid was attributed to the process holding its truncation: {:?}",
            report.snapshot.live
        );
        let problem = report
            .problems
            .first()
            .expect("the drift is stated, not swallowed");
        assert_eq!(problem.kind, ErrorKind::UnknownShape);
        assert!(matches!(problem.detail, ErrorDetail::Fields { .. }));

        // And nothing can be stopped through it: the failure to read is an error, never the
        // "already stopped" that would claim we looked and found the session gone.
        let err = service
            .stop_session(&claude_key(SID_A))
            .expect_err("cannot claim absence");
        assert_eq!(err.kind, ErrorKind::UnknownShape);
        assert!(h.recorder.sent().is_empty(), "nothing was signalled");
    }

    #[test]
    fn a_process_probe_failure_yields_no_rows_rather_than_unverified_ones() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "busy");
        let service = StatusService::with_roots(h.roots.clone()).with_probes(
            Arc::new(FakeProcesses {
                facts: vec![],
                fail: Some(ProbeFailure::Timeout),
            }),
            Arc::new(FakeFiles::default()),
            Arc::new(Recorder::default()),
        );

        let report = service.refresh();
        assert!(
            report.snapshot.live.is_empty(),
            "unverified is not the same as live"
        );
        assert_eq!(report.problems.len(), 1);
        assert_eq!(report.problems[0].kind, ErrorKind::Busy);
    }

    #[test]
    fn the_snapshot_is_cached_for_its_ttl_and_a_stop_invalidates_it() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "busy");
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        let first = service.snapshot();
        let second = service.snapshot();
        assert_eq!(
            first.snapshot.generated_at_ms, second.snapshot.generated_at_ms,
            "cached"
        );
        service.invalidate();
        // A cleared slot forces a real pass; the rows are the same, the stamp need not be.
        assert_eq!(service.snapshot().snapshot.live.len(), 1);
    }

    #[test]
    fn a_refresh_that_lands_late_cannot_overwrite_a_newer_snapshot_with_an_older_world() {
        let h = Harness::new();
        h.claude_file(4123, SID_A, "busy");
        let service = h.with_pids(&[(4123, 4100, "claude")]);

        // The newer world, observed now: one session live.
        let newer = service.refresh();
        assert_eq!(newer.snapshot.live.len(), 1);
        let newer_ms = newer.snapshot.generated_at_ms;

        // The older world, shaped as a slower concurrent refresh would leave it: begun a second
        // earlier, finished second, and carrying a session list from before the one above began.
        let mut older = StatusReport::default();
        older.snapshot.generated_at_ms = newer_ms - 1_000;
        service.cache.store_observed(older, newer_ms - 1_000);
        assert_eq!(
            service.snapshot().snapshot.live.len(),
            1,
            "a late arrival with an older observation overwrote the newer world"
        );

        // The converse, so this is ordering and not a refusal to ever store again: a pass that
        // observed later does land. It also pins `refresh` to `store_observed` — with `store`'s
        // `i64::MAX` stamp in the slot, no real observation could ever replace it.
        let mut newest = StatusReport::default();
        newest.snapshot.generated_at_ms = newer_ms + 1_000;
        service.cache.store_observed(newest, newer_ms + 1_000);
        assert!(
            service.snapshot().snapshot.live.is_empty(),
            "a newer observation must still be allowed to land"
        );
    }

    // -- 11: the real machine ------------------------------------------------------------------

    /// Runs against the real roots on whatever host is executing the suite and prints what it
    /// found. It asserts only invariants that must hold on *any* inventory — never a count, since
    /// the inventory changes between runs. Returns early when the roots are absent.
    ///
    /// Ran on this Mac (the owner's, macOS 24.0.0) 2026-09-13 with four live `claude` CLIs, two
    /// live `codex` threads and two live `opencode` processes.
    #[test]
    fn a_real_data_smoke_test_reports_what_this_machine_is_actually_running() {
        let roots = StatusRoots::real();
        if !roots.claude_sessions.is_dir() && !roots.codex_locks.is_dir() {
            println!("status smoke: no engine roots on this host — skipped");
            return;
        }
        let service = StatusService::new();
        let report = service.refresh();
        let counts = report.snapshot.counts();
        println!(
            "status smoke: {} live | running={} needs_you={} finished={} unknown={}",
            report.snapshot.live.len(),
            counts.running,
            counts.needs_you,
            counts.finished,
            counts.unknown
        );
        for problem in &report.problems {
            println!(
                "  problem: {:?} {:?} — {}",
                problem.provider, problem.kind, problem.message
            );
        }
        for obs in &report.snapshot.live {
            println!(
                "  {} pid={:?} {:?} word={:?}",
                obs.key.id(),
                obs.pid,
                obs.state,
                obs.raw_word
            );
            for line in &obs.evidence {
                println!("      · {line}");
            }
        }

        for obs in &report.snapshot.live {
            assert!(obs.key.is_valid(), "a live row must have a usable id");
            assert_eq!(
                obs.process,
                ProcessPresence::Present,
                "only live rows reach `live`"
            );
            assert!(
                !obs.evidence.is_empty(),
                "every observation must be checkable"
            );
            assert!(obs.pid.is_some(), "a live row names the process it saw");
            if obs.key.provider_id == ProviderId::Codex {
                assert_eq!(
                    obs.key.sid.len(),
                    36,
                    "a codex thread id is never truncated"
                );
            }
        }
        let mut keys: Vec<String> = report.snapshot.live.iter().map(|o| o.key.id()).collect();
        let before = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), before, "one row per session key");
        assert_eq!(
            counts.running + counts.needs_you + counts.unknown + counts.finished,
            report.snapshot.live.len() as u32,
            "every live row lands in exactly one count"
        );
    }

    /// Two live processes on one session is ordinary: resuming in a second terminal — which
    /// Pigeon's own Resume button does — produces exactly this. Measured here today, pids 3975
    /// and 56461 both naming session `460e1a93`, one `busy` and one `idle`.
    fn on_one_session(state: LiveState, since_ms: i64, pid: u32, word: &str) -> LiveObservation {
        LiveObservation {
            key: SessionKey::new(
                ProviderId::ClaudeCode,
                "460e1a93-2c29-4679-9ea0-a95f263ce79d",
            ),
            process: ProcessPresence::Present,
            state,
            since_ms: Some(since_ms),
            raw_word: Some(word.to_string()),
            evidence: vec![format!("pid {pid} says {word}")],
            pid: Some(pid),
            console_id: None,
            active_subagents: 0,
            observed_at_ms: 9,
        }
    }

    #[test]
    fn a_session_whose_second_terminal_went_idle_is_still_running_if_the_first_is_working() {
        // Recency alone answered a question about a PROCESS. The owner is asking about the
        // session, and a session with a working process is working.
        let collapsed = collapse(vec![
            on_one_session(LiveState::Running, 100, 3975, "busy"),
            on_one_session(LiveState::NeedsYou, 500, 56461, "idle"),
        ]);

        assert_eq!(collapsed.len(), 1, "one session, one row");
        assert_eq!(collapsed[0].state, LiveState::Running);
        assert_eq!(
            collapsed[0].pid,
            Some(3975),
            "the row names the process that justifies it"
        );
        assert_eq!(
            collapsed[0].evidence.len(),
            2,
            "and both processes are still accounted for"
        );
    }

    #[test]
    fn recency_still_breaks_a_tie_between_two_processes_in_the_same_state() {
        let collapsed = collapse(vec![
            on_one_session(LiveState::NeedsYou, 100, 111, "idle"),
            on_one_session(LiveState::NeedsYou, 500, 222, "idle"),
        ]);

        assert_eq!(collapsed.len(), 1);
        assert_eq!(collapsed[0].pid, Some(222), "the more recent of two equals");
    }

    #[test]
    fn a_process_we_cannot_read_never_outranks_one_we_can() {
        let collapsed = collapse(vec![
            on_one_session(LiveState::Unknown, 900, 333, "something-new"),
            on_one_session(LiveState::NeedsYou, 100, 444, "idle"),
        ]);

        assert_eq!(collapsed[0].state, LiveState::NeedsYou);
        assert_eq!(collapsed[0].pid, Some(444));
    }

    #[test]
    fn two_different_sessions_are_never_collapsed_into_one() {
        let mut other = on_one_session(LiveState::Running, 1, 555, "busy");
        other.key = SessionKey::new(ProviderId::Codex, "01a08d01-cea6-7d43-8552-dfbb944db822");
        let collapsed = collapse(vec![
            on_one_session(LiveState::NeedsYou, 1, 666, "idle"),
            other,
        ]);

        assert_eq!(collapsed.len(), 2);
    }
}
