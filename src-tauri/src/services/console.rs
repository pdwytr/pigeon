//! The console runtime: the owner's own engine CLIs, hosted in pseudo-terminals.
//!
//! Ported from Demo Studio's `app/src-tauri/src/console.rs` (cli-hosting lane A1), which has
//! run in production on Windows for months and had **never once run on macOS** — it compiled there
//! and nothing more. What survived the port is listed at the bottom of this comment; the reasoning
//! that survived with it is kept inline, function by function, because it was all paid for.
//!
//! **The bright line.** This module LAUNCHES the owner's own installed CLI and never bundles,
//! vendors or redistributes one: the program is resolved on PATH at spawn time
//! ([`resolve_on_path`]) and a missing engine is a named refusal, not an OS error code from four
//! layers down. Nothing here reads or forwards a credential — auth stays inside the engine's own
//! client, in the home its own environment names.
//!
//! -- LOCKING (the rule most easily broken in this file) -----------------------------------------
//!
//! The registry mutex is held for ONE map mutation at a time and for nothing else. It is never
//! held across a spawn, a PTY write, a kill, a `wait`, or an emit. The mechanism is that a
//! `Console` owns no I/O object directly — it owns `Arc`s to them. Every method therefore reads
//! the registry, clones the one `Arc` it needs, RELEASES the registry lock, and only then touches
//! the device:
//!
//! ```text
//! resolve -> lock -> read/mutate the map -> unlock -> do the work
//! ```
//!
//! That matters most for [`ConsoleService::input`]. A PTY write blocks indefinitely once the child
//! stops draining its input (a unix pty's input buffer is a few kilobytes), and holding a
//! process-wide registry lock across it would wedge every other console, `console_list` and app
//! shutdown behind one stuck CLI. The per-console writer mutex it takes instead serialises writes
//! to that one pty and nothing else. The only nested holds in this file are around single syscalls
//! (`resize`, `kill`) — atoms, and on a per-console mutex rather than the registry.
//!
//! Every mutex here is a non-reentrant `std::sync::Mutex`: a second acquisition on one thread does
//! not warn, it deadlocks permanently. [`lock`] is poison-tolerant for the reason stated there.
//!
//! -- THE ATTACH HANDSHAKE -----------------------------------------------------------------------
//!
//! A console's output pump does not start until the View says a terminal is attached
//! ([`ConsoleService::ready`]). On Windows that ordering is not a nicety: a fresh ConPTY's first
//! act is to write `ESC [ 6 n` and suspend the child until a cursor report comes back, and a query
//! delivered into a void leaves the console silent forever (Studio measured this on 2026-08-27). A
//! unix pty asks no such question — the child runs immediately — but the gate is kept anyway and
//! kept FIRST, because it is also what makes the View's documented attach order work: subscribe,
//! create the terminal, fit, resize, list, replay scrollback, and only then `console_ready`.
//! Without it, the bytes a console emits between `console_open` and the listener being registered
//! are simply gone. See [`PumpGate`].
//!
//! -- WHAT CHANGED ON THE WAY OVER FROM STUDIO ---------------------------------------------------
//!
//! *Kept:* [`PumpGate`], [`pump`], [`supervise`], [`kill_child`], [`resolve_on_path`],
//! [`launch_argv`], [`pty_size`], [`normalize_sid`] and the whole locking discipline above.
//!
//! *Cut:* Studio's closed `ConsoleCommand` set, its profile/env launch identities and the spawn
//! logging that went with them. Pigeon has no profiles and no closed command set, so `profile`,
//! `profileId`, `command` and `env` are gone from the console, the row and the exit payload.
//!
//! *Changed:* Studio hardcodes one program (`claude`) and one argv shape (`--resume <sid>`).
//! Pigeon is multi-engine, so both come from [`ProviderId::program`] and
//! [`ProviderId::resume_args`], and the provider is stored on the console and echoed on its
//! summary.
//!
//! *Added:* a bounded scrollback ring (Studio's View kept its own and never reattached to a
//! detached console), a `mode` that distinguishes a resume from a new session, the unix half of
//! [`supervise`] (its EOF arrives by a different route and its master must be closed in the other
//! order), and the gate release in `close`/`close_all` that a macOS child blocked in the tty drain
//! turns out to need — see [`PumpGate`] for that measurement, which is the one thing this port
//! found that Windows had never been able to show.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use std::{collections::HashMap, thread};

use crate::api::errors::{ApiError, ApiErrorCode, EngineError, ErrorKind};
use crate::api::events::{ConsoleData, ConsoleExit, CONSOLE_DATA, CONSOLE_EXIT};
use crate::api::types::{ConsoleListResult, ConsoleScrollbackDto, ConsoleSummaryDto};
use crate::domain::{ProviderId, SessionKey};
use crate::pathenv;
use crate::util::now_ms;

/// The largest `console://data` payload, before base64 expands it by 4/3.
///
/// A bound rather than a tuning knob: a build that dumps a megabyte of output in one burst must
/// not be able to assemble it into a single event, because the whole chunk has to be encoded,
/// cloned through the IPC bridge and decoded before ANY of it can be drawn. 8 KB keeps a torrent
/// streaming instead of arriving as one late slab. There is no lower bound and none is needed —
/// short reads are emitted as they arrive, since a prompt that appears in two events still
/// appears.
pub const MAX_CHUNK: usize = 8 * 1024;

/// How much recent output one console keeps for replay, in bytes.
///
/// **256 KB, and the number is chosen against what it is for**: a terminal the owner detached from
/// and came back to. An 80x24 screen is ~2 KB of text, so 256 KB is on the order of a hundred
/// screens — more than enough to show what an engine was doing while the owner was looking
/// elsewhere, and enough to survive one `ls -R` or a stack trace without losing the prompt above
/// it. The ceiling matters more than the depth: this is per console, held in the host process, and
/// `console_scrollback` base64s the tail before it crosses the bridge (4/3 on top). Ten detached
/// consoles at this cap is 2.5 MB resident and a ~340 KB reply at worst, which is a cost the owner
/// never notices. A megabyte each would be 10 MB and a 1.4 MB reply per reattach, which they
/// would.
pub const SCROLLBACK_CAP: usize = 256 * 1024;

/// Terminal dimensions are clamped into `[MIN_DIM, MAX_DIM]` before they reach the pty.
///
/// The floor exists because a fit-addon that measures before layout reports 0, and a 0-column
/// terminal is a spawn failure rather than a small one. The ceiling was Studio's guard against
/// `CreatePseudoConsole`'s `COORD`, whose fields are **i16** — a number past 32767 wraps negative
/// on the way in. It is kept off Windows for a smaller but real reason: `TIOCSWINSZ` takes a `u16`
/// and will cheerfully accept an absurd size, and every curses program in the child then computes
/// its layout against a screen that does not exist. 5000 is far beyond any real display (a 4K
/// monitor at a 5 px cell is ~768 columns).
pub const MIN_DIM: u32 = 1;
/// See [`MIN_DIM`].
pub const MAX_DIM: u32 = 5000;

/// How long [`supervise`] waits for the output pump to drain before it stops waiting.
///
/// See the unix note in [`supervise`]: a console's exit must never be hostage to a grandchild that
/// inherited the pty and outlived its parent.
const PUMP_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// The registry map, shared between the service and each console's supervisor thread.
type Registry = Arc<Mutex<HashMap<String, Console>>>;

/// The write half of one pty master. Its own mutex so a blocking write serialises against that one
/// console and never against the registry (see the locking note at the top of this file).
type WriterSlot = Arc<Mutex<Box<dyn Write + Send>>>;

/// The pty master, `None` once the child has exited and the supervisor has closed it. A resize
/// against `None` is a no-op rather than an error — see [`ConsoleService::resize`].
type MasterSlot = Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>;

/// A handle that can terminate the child, cloned from it at spawn so the registry never has to
/// share ownership of the `Child` itself (the supervisor thread is blocked in `wait()` on that).
type KillerSlot = Arc<Mutex<Box<dyn ChildKiller + Send + Sync>>>;

/// The recent-output ring for one console, behind its own mutex for the same reason the writer is.
type ScrollbackSlot = Arc<Mutex<Scrollback>>;

/// Poison-tolerant lock. Every critical section in this file is one map mutation or one syscall,
/// so a poisoned mutex can only mean a panic somewhere else in the process. Treating that as
/// "consoles are unusable forever" would turn an unrelated crash into a dead terminal surface with
/// a live engine CLI behind it.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// -- the attach gate ----------------------------------------------------------------------------

/// A one-way latch. The pump thread waits on it, [`ConsoleService::ready`] opens it, it never
/// closes again.
///
/// Studio's reasoning, which is why this is unconditional rather than `#[cfg(windows)]`: a fresh
/// ConPTY writes `ESC [ 6 n` (DSR-CPR) and suspends the child until a cursor report arrives back
/// on the pty input. Nothing in the host can answer that safely — an attached terminal answers it
/// too, and the second report would reach the CLI as keystrokes. So the answer has to come from
/// the terminal, and the defect was never that the query went unanswered but that it could be
/// DELIVERED before anything existed to answer it. Gating delivery closes that at the one point
/// where the ordering is knowable.
///
/// **On macOS the gate holds the child too, for an entirely unrelated reason, and this was
/// measured on this Mac on 2026-09-13 — the first time this code had ever run on the platform.** A
/// unix pty asks no cursor question, so the child does start running immediately; what it cannot
/// do is FINISH. A process whose controlling terminal still holds unread output blocks in the
/// kernel's tty drain on the last close of that terminal, and that close happens inside `exit(2)`.
/// Measured: a child that printed one line and exited was still unreaped after 3.1 s with the gate
/// shut, and completed within milliseconds of it opening. A child that printed NOTHING exited
/// immediately, which is what identifies the drain rather than anything about the spawn.
///
/// Two consequences, both load-bearing:
///
///   1. the gate withholding output is observable here rather than a Windows nicety — "a console
///      that is never readied produces nothing and ends nothing" is true on both platforms, by two
///      mechanisms that have nothing to do with each other;
///   2. **a drain-blocked child cannot be signalled out of it.** It is already inside `exit`, so
///      the SIGHUP [`kill_child`] sends reaches nothing. The only way out is to let the pump drain
///      the terminal, which is why [`ConsoleService::close`] and [`ConsoleService::close_all`]
///      open the gate as well as killing. Measured before they did: closing a never-readied chatty
///      console produced no exit event in 4 seconds and left the child alive.
///
/// The gate is ALSO opened unconditionally during teardown ([`supervise`]), which is load-bearing
/// rather than tidy: a console closed before it was ever readied would otherwise leave its pump
/// parked here forever. One latch, several openers, and no path that leaves a thread waiting on a
/// signal that can no longer come.
///
/// `Debug` because `portable_pty::Child` requires it of anything holding one — the tests' scripted
/// child parks on a second gate, which is the only way to drive [`supervise`] without a real pty.
#[derive(Default, Debug)]
struct PumpGate {
    opened: Mutex<bool>,
    signal: std::sync::Condvar,
}

impl PumpGate {
    /// Open the gate and release every waiter. Idempotent — `console_ready` promises it is.
    fn open(&self) {
        *lock(&self.opened) = true;
        self.signal.notify_all();
    }

    /// Block until the gate is open; return at once if it already is.
    fn wait(&self) {
        let mut opened = lock(&self.opened);
        while !*opened {
            opened = self.signal.wait(opened).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Test-only, so that "the gate is open" can be asserted rather than demonstrated by a `wait`
    /// that returns — a regression there would hang the run instead of failing it.
    #[cfg(test)]
    fn is_open(&self) -> bool {
        *lock(&self.opened)
    }
}

// -- bounded scrollback -------------------------------------------------------------------------

/// A byte ring of one console's recent output, capped at [`SCROLLBACK_CAP`].
///
/// Bytes, not characters or lines: what the View replays into xterm.js is a byte stream including
/// every escape sequence, and a ring that split on newlines would drop half of a
/// cursor-positioning sequence and leave the terminal in a mode nothing set.
#[derive(Default)]
struct Scrollback {
    buf: VecDeque<u8>,
    /// True once anything has been evicted, so the reply can say `truncated` even when the caller
    /// asked for everything.
    evicted: bool,
}

impl Scrollback {
    fn push(&mut self, chunk: &[u8]) {
        // A single chunk larger than the cap keeps only its tail, so the copy below is bounded by
        // the cap rather than by whatever the child wrote in one burst.
        let chunk = if chunk.len() > SCROLLBACK_CAP {
            self.evicted = true;
            &chunk[chunk.len() - SCROLLBACK_CAP..]
        } else {
            chunk
        };
        self.buf.extend(chunk.iter().copied());
        if self.buf.len() > SCROLLBACK_CAP {
            let excess = self.buf.len() - SCROLLBACK_CAP;
            self.buf.drain(..excess);
            self.evicted = true;
        }
    }

    fn len(&self) -> usize {
        self.buf.len()
    }

    /// The last `max_bytes` bytes (or everything, when `None`), and whether anything was left out.
    ///
    /// **The cut is moved FORWARD to a UTF-8 character boundary, and this is not cosmetic.** The
    /// View writes these bytes straight into a terminal renderer: half of a multi-byte character
    /// makes xterm.js draw a replacement glyph, and half of one at the START of a replay puts the
    /// renderer one byte out of step with everything that follows it. Eviction inside the ring is
    /// deliberately left raw and byte-exact — the boundary is repaired here, once, at the only
    /// place bytes leave this type, so there is exactly one implementation of the rule to be
    /// wrong.
    ///
    /// Only the front is trimmed. A partial character at the END is the live stream's own tail:
    /// the continuation bytes are still coming, xterm.js buffers them across writes, and cutting
    /// them here would break the character that the next live chunk completes.
    fn tail(&self, max_bytes: Option<usize>) -> (Vec<u8>, bool) {
        let bytes: Vec<u8> = self.buf.iter().copied().collect();
        let wanted = max_bytes.unwrap_or(bytes.len()).min(bytes.len());
        let mut start = bytes.len() - wanted;
        // A UTF-8 continuation byte is 0b10xxxxxx; a boundary is anything else. Advancing lands on
        // the first byte of the next whole character, or on the end of the buffer.
        while start < bytes.len() && (bytes[start] & 0xC0) == 0x80 {
            start += 1;
        }
        let truncated = self.evicted || start > 0;
        (bytes[start..].to_vec(), truncated)
    }
}

// -- the console's own vocabulary ---------------------------------------------------------------

/// How a console was started. `resume` carries a [`SessionKey`]; `new` does not yet have one.
///
/// The distinction is not cosmetic: an engine writes its session record when it feels like it, so
/// a console started fresh has no discoverable id until the engine writes one. The mode records
/// what the owner asked for, which is a fact that never changes; [`ConsoleService::link_session`]
/// fills in the key when the record shows up.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsoleMode {
    Resume,
    New,
}

impl ConsoleMode {
    /// The wire value, which the contract types as `"resume" | "new"`.
    pub fn as_str(self) -> &'static str {
        match self {
            ConsoleMode::Resume => "resume",
            ConsoleMode::New => "new",
        }
    }

    /// A word from the View. An unknown one is refused rather than defaulted — defaulting to `new`
    /// would silently drop a resume target and start a second session on the same folder.
    pub fn parse(word: &str) -> Result<Self, ApiError> {
        match word {
            "resume" => Ok(ConsoleMode::Resume),
            "new" => Ok(ConsoleMode::New),
            other => Err(ApiError::invalid(format!(
                "console mode must be \"resume\" or \"new\", not {other:?}"
            ))),
        }
    }
}

/// What a console is doing, as the contract's four words.
///
/// `starting` and `running` are separated by the attach gate rather than by the child: a console
/// whose terminal has not attached yet is `starting`, whatever the process is doing, because that
/// is the state the owner can act on (their terminal has not come up).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConsoleState {
    Starting,
    Running,
    Exited,
}

impl ConsoleState {
    fn as_str(self) -> &'static str {
        match self {
            ConsoleState::Starting => "starting",
            ConsoleState::Running => "running",
            ConsoleState::Exited => "exited",
        }
    }
}

/// Where a console's events go. A trait rather than a bare `AppHandle` for one reason: a `State`,
/// a `WebviewWindow` and an `AppHandle` all need a running Tauri app to exist, so a service that
/// named one could only be tested by starting the app. Everything in this file that matters — the
/// gate, the ordering, the exactly-once exit, the real pty — is then testable against a recording
/// implementation, which is how the smoke test at the bottom can assert what arrives.
pub trait ConsoleEvents: Send + Sync + 'static {
    fn data(&self, payload: ConsoleData);
    fn exit(&self, payload: ConsoleExit);
}

impl ConsoleEvents for tauri::AppHandle {
    fn data(&self, payload: ConsoleData) {
        // The result is dropped for the reason Studio drops it: an emit fails when there is no
        // window left to receive it, which is the ordinary state during shutdown and not an error
        // anything can act on.
        let _ = tauri::Emitter::emit(self, CONSOLE_DATA, payload);
    }

    fn exit(&self, payload: ConsoleExit) {
        let _ = tauri::Emitter::emit(self, CONSOLE_EXIT, payload);
    }
}

/// One hosted console. Holds no I/O object by value — see the locking note at the top of the file.
struct Console {
    /// Open order, so the listing is deterministic and reads the way the owner opened them (a
    /// lexicographic sort on the id would put `c10` between `c1` and `c2`).
    seq: u64,
    provider: ProviderId,
    /// The session this console is attached to. `None` for a `new` console until the engine writes
    /// a discoverable record and something calls [`ConsoleService::link_session`].
    session_key: Option<SessionKey>,
    cwd: String,
    mode: ConsoleMode,
    cols: u16,
    rows: u16,
    started_at_ms: i64,
    writer: WriterSlot,
    master: MasterSlot,
    killer: KillerSlot,
    scrollback: ScrollbackSlot,
    /// Opened by [`ConsoleService::ready`]; until then this console's output stays in the pty.
    gate: Arc<PumpGate>,
    /// Set when the gate is opened, so `starting` and `running` can be told apart.
    readied: bool,
    running: bool,
    exit_code: Option<i32>,
}

impl Console {
    fn state(&self) -> ConsoleState {
        match (self.running, self.readied) {
            (false, _) => ConsoleState::Exited,
            (true, true) => ConsoleState::Running,
            (true, false) => ConsoleState::Starting,
        }
    }

    fn summary(&self, id: &str, scrollback_bytes: usize) -> ConsoleSummaryDto {
        ConsoleSummaryDto {
            id: id.to_string(),
            session_key: self.session_key.clone(),
            provider: self.provider,
            cwd: self.cwd.clone(),
            mode: self.mode.as_str().to_string(),
            state: self.state().as_str().to_string(),
            cols: self.cols,
            rows: self.rows,
            scrollback_bytes,
            exit_code: self.exit_code,
            started_at_ms: self.started_at_ms,
        }
    }
}

// -- pure helpers (everything that can be tested without a device, is) ---------------------------

/// Mint a console id and its open-order key, which are the same number.
///
/// **Unique within the process, and deliberately no stronger than that.** The registry does not
/// survive a restart, so there is nothing for a later process to collide WITH — a consumer that
/// persisted an id across a restart would be holding a handle to a console that no longer exists,
/// and a globally-unique id would hide that rather than fix it.
fn mint_id() -> (u64, String) {
    let seq = next_seq();
    (seq, format!("c{seq}"))
}

fn next_seq() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Base64 for the wire. Standard alphabet with padding, both directions.
fn encode_b64(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

/// The source error is dropped rather than formatted in, per `api::errors`' first rule: an error
/// built from another crate's text is how a path or a payload ends up in a message.
fn decode_b64(s: &str) -> Result<Vec<u8>, ApiError> {
    BASE64
        .decode(s)
        .map_err(|_| ApiError::invalid("console input: dataB64 is not valid base64"))
}

/// A resume target, or nothing. Blank and whitespace-only ids are NOT resume targets.
///
/// The View can honestly send either `null` or `""` for a session it has not chosen yet, and
/// normalising here means one answer for both, in one place. The alternative is `claude --resume
/// ""` failing inside the CLI with a message about an empty session id — a confusing report of a
/// bug that is entirely ours. Pigeon's version trims the id out of a whole [`SessionKey`],
/// because a key is what a resume names here.
fn normalize_sid(sid: Option<&str>) -> Option<String> {
    sid.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The engine's own arguments for this console: its resume invocation, or nothing at all.
///
/// Studio hardcoded `["--resume", sid]`. Pigeon asks the provider, because the three engines
/// genuinely disagree — `claude --resume <id>`, `codex resume <id>`, `opencode --session <id>` —
/// and an argv guessed from one engine's shape would start the wrong thing on another.
fn launch_args(provider: ProviderId, mode: ConsoleMode, sid: Option<&str>) -> Vec<String> {
    match (mode, normalize_sid(sid)) {
        (ConsoleMode::Resume, Some(sid)) => provider.resume_args(&sid),
        _ => Vec::new(),
    }
}

/// Clamp a requested terminal size into what a pty will accept. See [`MIN_DIM`].
fn pty_size(cols: u32, rows: u32) -> PtySize {
    let clamp = |v: u32| v.clamp(MIN_DIM, MAX_DIM) as u16;
    PtySize {
        rows: clamp(rows),
        cols: clamp(cols),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// The executable extensions a bare program name may pick up on this platform.
#[cfg(windows)]
fn executable_extensions(pathext: Option<&OsStr>) -> Vec<String> {
    let raw = pathext
        .and_then(|v| v.to_str())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(".COM;.EXE;.BAT;.CMD");
    raw.split(';')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| {
            if e.starts_with('.') {
                e.to_string()
            } else {
                format!(".{e}")
            }
        })
        .collect()
}

#[cfg(not(windows))]
fn executable_extensions(_pathext: Option<&OsStr>) -> Vec<String> {
    Vec::new()
}

/// A candidate is only a hit if it is a file this process could actually execute.
///
/// **The one behaviour change inside the ported resolver, and it is a unix one.** Windows decides
/// executability by extension, which `executable_extensions` already covers. Unix decides it by
/// the mode bits, and `is_file()` alone would happily resolve a `claude` that is somebody's notes:
/// the spawn then fails with a permission error naming a path the owner did not think was on their
/// PATH. Checking the bit here turns that into "the CLI was not found", which is the truth and is
/// the message that tells them what to do.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Find `program` on `path_var` — a `which`-equivalent, kept a pure function of its inputs so the
/// resolution order is testable against a temp directory instead of the machine's real PATH.
///
/// **Pigeon resolves the CLI itself rather than leaving it to the spawn**, for the three reasons
/// Studio found in `portable-pty`'s own path, all of which hold on unix too:
///
///   1. `CommandBuilder`'s own search falls back to returning the bare name when it finds nothing,
///      so a missing `claude` becomes a spawn failure with an OS error code, several layers below
///      anything that could say "the CLI is not installed";
///   2. the resolved path is what a report of "it launched the wrong `claude`" turns on;
///   3. on Windows, the answer decides whether the launch needs the `cmd.exe` shim below.
///
/// An exact hit on the given name wins over any extension, which is what makes an absolute path
/// passed as `program` resolve to itself.
fn resolve_on_path(
    program: &str,
    path_var: Option<&OsStr>,
    pathext: Option<&OsStr>,
) -> Option<PathBuf> {
    let path_var = path_var?;
    let extensions = executable_extensions(pathext);
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let exact = dir.join(program);
        if is_executable_file(&exact) {
            return Some(exact);
        }
        for ext in &extensions {
            // Built by concatenation rather than `with_extension`, which REPLACES an existing
            // suffix: a program name that happens to contain a dot would otherwise be truncated.
            let mut name = OsString::from(program);
            name.push(ext);
            let candidate = dir.join(&name);
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Put the console child's environment on top of the inherited one, minus any parent Claude
/// session's markers (see [`pathenv::INHERITED_SESSION_MARKERS`] for why they cannot pass).
fn apply_child_env(builder: &mut CommandBuilder, overrides: Vec<(String, String)>) {
    for name in pathenv::INHERITED_SESSION_MARKERS {
        builder.env_remove(name);
    }
    for (name, value) in overrides {
        builder.env(name, value);
    }
}

/// The argv to hand the spawn, given a resolved executable and the engine's own arguments.
///
/// **On Windows this is not a formality** and the note is kept because the port is meant to run
/// there too one day: `claude` installs two ways, and `CreateProcessW` cannot execute the `.cmd`
/// that `npm -g` drops — it fails with ERROR_BAD_EXE_FORMAT (193), an error whose text says
/// nothing about shims. So a batch shim is launched through the command processor instead, and the
/// quoting survives that hop only because the sole argument that can need quoting is the
/// executable path (`cmd /c` preserves quotes only around exactly two of them). A session id is a
/// uuid and the resume flags are literals, so nothing else can ever need it — and an argument with
/// a space in it must not be added without revisiting this.
///
/// On unix there is no shim concept: the kernel reads the shebang, and `comspec` is ignored.
fn launch_argv(exe: &Path, args: &[String], comspec: Option<&OsStr>) -> Vec<OsString> {
    let mut argv: Vec<OsString> = Vec::with_capacity(args.len() + 3);
    #[cfg(windows)]
    {
        let is_batch = exe
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
            .unwrap_or(false);
        if is_batch {
            argv.push(comspec.unwrap_or(OsStr::new("cmd.exe")).to_os_string());
            argv.push(OsString::from("/c"));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = comspec; // no shim concept off Windows
    }
    argv.push(exe.as_os_str().to_os_string());
    argv.extend(args.iter().map(OsString::from));
    argv
}

// -- the pump and the supervisor -----------------------------------------------------------------

/// Read a pty master to EOF, handing each chunk to `on_data` in order.
///
/// Kept free of Tauri so the chunk bound is a unit test rather than an assertion about a running
/// app. A read error ends the pump the same way EOF does: by the time the master errors, the
/// console is over, and the process's real exit status is the supervisor's business, not this
/// loop's — reporting a torn pipe as a second, competing ending would put two stories in the log.
fn pump(mut reader: Box<dyn Read + Send>, mut on_data: impl FnMut(&[u8])) {
    let mut buf = vec![0u8; MAX_CHUNK];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => on_data(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

/// Run one console to completion: pump its output, wait for the child, report the ending exactly
/// once, in that order. Blocks; call it on its own thread.
///
/// **Why the wait and the pump are separate threads.** Studio's Windows reason: a ConPTY's output
/// pipe does not reach EOF just because the child exited — the write end belongs to the
/// pseudoconsole, which lives until `ClosePseudoConsole` runs in the master's `Drop`. So a single
/// thread that read to EOF and then waited would wait forever on a child that had already gone.
///
/// **The unix half is the part Studio never needed, and it inverts that.** Here the pump's reader
/// is a *dup* of the master fd (`UnixMasterPty::try_clone_reader`), so dropping the master does
/// nothing for it; what ends the read is the SLAVE side closing, which `portable-pty` turns from
/// EIO into a clean `Ok(0)`. That happens when the last holder of the slave exits — normally the
/// child, at exactly the moment `wait()` returns. So on unix this waits for the pump to drain on
/// its own, bounded by [`PUMP_DRAIN_GRACE`], and then stops waiting: a grandchild that inherited
/// the pty and outlived its parent must not be able to hold a console's exit event hostage. When
/// the grace expires the pump thread is detached rather than joined, and any late output from that
/// lingering process is still emitted — it is still that console's output.
///
/// **And why the pump is drained before `on_exit`.** The contract promises output chunks in order
/// and one exit at the end of them. Emitting the exit straight off the `wait` would race the last
/// few chunks still in flight, so a console's final line could arrive after its own obituary.
///
/// The registry is not touched here at all. `on_exit` receives the code and decides — which keeps
/// the one function that must not hold a lock across `wait()` structurally unable to.
///
/// **The pump starts parked on `gate`** ([`PumpGate`] carries the reasoning): output is not read
/// out of the pty until the View has a terminal attached.
fn supervise(
    mut child: Box<dyn Child + Send + Sync>,
    reader: Box<dyn Read + Send>,
    master: MasterSlot,
    gate: Arc<PumpGate>,
    on_data: impl FnMut(&[u8]) + Send + 'static,
    on_exit: impl FnOnce(Option<i32>),
) {
    let (drained_tx, drained_rx) = std::sync::mpsc::channel::<()>();
    let pump_gate = gate.clone();
    let pump_thread = thread::spawn(move || {
        pump_gate.wait();
        pump(reader, on_data);
        let _ = drained_tx.send(());
    });

    // `null` when the code is unobtainable, which the contract allows for explicitly.
    let code = child.wait().ok().map(|s| s.exit_code() as i32);

    // Release a pump that was never readied. Without this, a console closed before its terminal
    // ever attached would leave that thread parked on a signal nothing can now send, and the drain
    // below would wait out its whole grace for nothing. Idempotent, so a readied console is
    // unaffected.
    gate.open();

    // Unix: the child's exit has already closed the slave, so the pump is finishing by itself
    // right now. Wait for it BEFORE the master is dropped — the drop closes an fd, and closing one
    // while another thread may be about to read it is how a console ends up reading whatever the
    // OS handed that number to next.
    #[cfg(unix)]
    let drained = drained_rx.recv_timeout(PUMP_DRAIN_GRACE).is_ok();

    // Taken under the lock, dropped outside it: on Windows the drop is `ClosePseudoConsole`, which
    // flushes and can block on a slow reader — precisely the kind of work the locking rule keeps
    // out of a hold. It is also what turns the child's exit into the pump's EOF there.
    let closing = { lock(&master).take() };
    drop(closing);

    #[cfg(windows)]
    let drained = drained_rx.recv_timeout(PUMP_DRAIN_GRACE).is_ok();

    if drained {
        // Returns at once — the pump has already finished; this only reaps the thread.
        let _ = pump_thread.join();
    }
    on_exit(code);
}

/// Best-effort terminate.
///
/// **This is a SIGHUP on unix, not a `TerminateProcess`, and the difference is worth knowing.**
/// `portable-pty` 0.9.0's `clone_killer` hands back a `ProcessSignaller` that sends `SIGHUP`
/// (`lib.rs:327`) — the Child's own SIGKILL is not reachable from here, because the supervisor
/// thread owns the `Child` while blocked in `wait()`. A hangup is the honest signal for closing a
/// terminal and every well-behaved CLI exits on it, but two cases do not end here: a child that
/// ignores SIGHUP, and a child already blocked in the macOS tty drain (see [`PumpGate`]) where no
/// signal is delivered at all. The second is handled by opening the gate alongside the kill; the
/// first would need an escalation to SIGKILL, which needs `libc` as a direct dependency.
///
/// **The result is deliberately discarded, and this is not laziness.** `portable-pty`'s Windows
/// `kill` inverts its own success test (`TerminateProcess` returns non-zero on SUCCESS, and that
/// arm returns `Err(last_os_error())`), so a kill that worked reports an error; on unix a kill of
/// a process that has already exited is an `ESRCH` meaning "already in the state you asked for".
/// `close` converges on "closed" either way, so there is nothing this result could correctly
/// decide.
fn kill_child(killer: &KillerSlot) {
    let _ = lock(killer).kill();
}

// -- the registry halves, as free functions ------------------------------------------------------

/// Which gate (if any) an id calls for, and the state change that goes with opening it.
///
/// `Err` is an unknown console, `Ok(None)` one that has already ended, `Ok(Some(_))` a live one. A
/// free function over the registry rather than a block inside the method, so the three-way answer
/// is tested against the code that ships instead of a copy of it.
///
/// The gate is returned rather than opened here: opening wakes a thread that immediately starts
/// reading a pty, and that thread must never come up holding, or waiting on, the registry lock.
fn gate_to_open(registry: &Registry, id: &str) -> Result<Option<Arc<PumpGate>>, ApiError> {
    let mut map = lock(registry);
    match map.get_mut(id) {
        None => Err(unknown_console(id)),
        Some(console) if !console.running => Ok(None),
        Some(console) => {
            console.readied = true;
            Ok(Some(console.gate.clone()))
        }
    }
}

/// The registry half of the listing, in open order.
fn list_rows(registry: &Registry) -> Vec<ConsoleSummaryDto> {
    // Two phases on purpose. The scrollback byte count lives behind each console's own mutex, and
    // taking that mutex while holding the registry would be a compound hold around work the pump
    // thread also wants — exactly the nesting the locking rule forbids. So the rows and their
    // scrollback handles come out under the registry lock, and the counts are read after it is
    // released.
    let mut rows: Vec<(u64, ConsoleSummaryDto, ScrollbackSlot)> = {
        let map = lock(registry);
        map.iter()
            .map(|(id, console)| {
                (
                    console.seq,
                    console.summary(id, 0),
                    console.scrollback.clone(),
                )
            })
            .collect()
    };
    rows.sort_by_key(|(seq, _, _)| *seq);
    rows.into_iter()
        .map(|(_, mut row, scrollback)| {
            row.scrollback_bytes = lock(&scrollback).len();
            row
        })
        .collect()
}

fn unknown_console(id: &str) -> ApiError {
    ApiError::not_found(format!("no console {id}")).with_detail("id", id)
}

// -- the service ---------------------------------------------------------------------------------

/// The console registry and the operations the command layer calls.
///
/// The map lives behind an `Arc` INSIDE this type rather than being the type, so a supervisor
/// thread can hold the registry directly instead of reaching back through an `AppHandle` for
/// managed state on every exit. One less thing that can be missing at shutdown.
pub struct ConsoleService {
    registry: Registry,
    events: Arc<dyn ConsoleEvents>,
    /// The PATH engine programs are resolved on. `None` means [`pathenv::effective_path`], which
    /// is every real launch; the tests point it at a scratch directory so the real path is
    /// exercised without the owner's real CLIs.
    path_var: Option<OsString>,
}

impl ConsoleService {
    /// The app's own service. Events go out as `console://data` and `console://exit`.
    pub fn new(app: tauri::AppHandle) -> Self {
        Self::with_events(Arc::new(app))
    }

    pub fn with_events(events: Arc<dyn ConsoleEvents>) -> Self {
        Self {
            registry: Registry::default(),
            events,
            path_var: None,
        }
    }

    #[cfg(test)]
    fn with_path_var(mut self, path: OsString) -> Self {
        self.path_var = Some(path);
        self
    }

    fn path_var(&self) -> Option<OsString> {
        match &self.path_var {
            Some(path) => Some(path.clone()),
            None => Some(pathenv::effective_path()),
        }
    }

    /// Open a console: a PATH-resolved engine CLI in a pty at `cwd`, resuming `session_key` when
    /// the mode says so. Returns the new console's id.
    ///
    /// **It returns before the child has drawn anything, and that is the design.** The output pump
    /// is parked until [`ConsoleService::ready`], so the View gets its id, mounts its terminal,
    /// subscribes, and only then lets the bytes flow. See [`PumpGate`]. The consequence a caller
    /// must expect: a console that is never readied produces nothing — and `close` still tears it
    /// down cleanly from there.
    ///
    /// Every failure is a typed [`ApiError`]; nothing here ever returns an id for a console that
    /// does not exist.
    pub fn open(
        &self,
        provider: ProviderId,
        session_key: Option<SessionKey>,
        cwd: &str,
        cols: u32,
        rows: u32,
        mode: ConsoleMode,
    ) -> Result<String, ApiError> {
        // **The mode and the key have to agree, and disagreement is refused rather than ranked.**
        // A caller that sent `mode: "new"` with a session key believes one of two different
        // things, and either silent winner is a console doing what its caller did not ask for:
        // resuming a session the caller meant to leave alone, or starting a second engine on a
        // session that already has one. Neither is a guess worth making.
        let session_key = match (mode, session_key) {
            (ConsoleMode::Resume, Some(key)) => {
                if !key.is_valid() {
                    return Err(ApiError::invalid(
                        "a resume console needs a usable session id",
                    ));
                }
                if key.provider_id != provider {
                    return Err(ApiError::invalid(format!(
                        "session {} belongs to {}, not {}",
                        key.sid,
                        key.provider_id.label(),
                        provider.label()
                    )));
                }
                Some(key)
            }
            (ConsoleMode::Resume, None) => {
                return Err(ApiError::invalid(
                    "a resume console needs a session to resume",
                ));
            }
            (ConsoleMode::New, Some(_)) => {
                return Err(ApiError::invalid(
                    "a new console starts no named session — its key is learned afterwards",
                ));
            }
            (ConsoleMode::New, None) => None,
        };

        // Checked here rather than left to the spawn, because `portable-pty` silently substitutes
        // the home directory for a cwd that is not one (Windows) or fails deep inside `pre_exec`
        // (unix). A console quietly running somewhere other than the folder the owner picked is
        // exactly the guessed-behaviour class this codebase refuses.
        let cwd_path = PathBuf::from(cwd);
        if !cwd_path.is_dir() {
            let err = ApiError::invalid(format!("not a directory: {cwd}"));
            return Err(err.with_detail("cwd", cwd));
        }

        let args = launch_args(provider, mode, session_key.as_ref().map(|k| k.sid.as_str()));
        let exe = resolve_on_path(
            provider.program(),
            self.path_var().as_deref(),
            std::env::var_os("PATHEXT").as_deref(),
        )
        .ok_or_else(|| {
            // The typed vocabulary rather than a sentence built here, so the View shows the same
            // words for "not installed" whichever surface found it out.
            ApiError::from(EngineError::of(provider, ErrorKind::NotInstalled))
                .with_detail("program", provider.program())
        })?;

        let argv = launch_argv(&exe, &args, std::env::var_os("ComSpec").as_deref());
        let mut builder = CommandBuilder::from_argv(argv);
        builder.cwd(&cwd_path);
        // The environment is INHERITED and then added to: `CommandBuilder::from_argv` seeds itself
        // from `std::env::vars_os`, which is what the CLI needs — its config, its credentials path
        // and its proxy settings all arrive that way. What `pathenv` puts on top is the PATH this
        // resolution actually used (so a tool the engine shells out to is found the same way) and
        // the terminal variables a unix pty has no other way to communicate.
        apply_child_env(&mut builder, pathenv::child_env());

        let pair = native_pty_system()
            .openpty(pty_size(cols, rows))
            .map_err(|_| ApiError::host("could not open a pseudo-terminal"))?;
        // The pty handles are taken BEFORE the spawn, deliberately. Both are pure master
        // operations that do not depend on a child existing, and taking them first means no
        // failure here can strand one: every `?` above leaves nothing running, and the only
        // fallible step after it is the spawn itself, which either produces a child this method
        // goes on to own or produces nothing at all. The other way round, a `take_writer` that
        // failed would return an error with a live CLI behind it and no id by which anything could
        // ever close it.
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|_| ApiError::host("could not read the pseudo-terminal"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|_| ApiError::host("could not write to the pseudo-terminal"))?;

        let child = pair.slave.spawn_command(builder).map_err(|_| {
            // The source error is dropped (it can quote an argv and a path); what the owner needs
            // is which program failed and where, and both are typed details.
            let what = format!("could not start {}", provider.label());
            ApiError::new(ApiErrorCode::HostFailure, what)
                .with_detail("program", exe.display().to_string())
                .with_detail("cwd", cwd)
        })?;
        // Dropped immediately, and load-bearing on both platforms: the slave is one end of the
        // pty, so a retained copy keeps it open past the child's death and the pump's EOF never
        // comes.
        drop(pair.slave);
        let killer = child.clone_killer();

        let (seq, id) = mint_id();
        let size = pty_size(cols, rows);
        let master: MasterSlot = Arc::new(Mutex::new(Some(pair.master)));
        let gate = Arc::new(PumpGate::default());
        let scrollback: ScrollbackSlot = Arc::new(Mutex::new(Scrollback::default()));
        {
            // The whole of the critical section: one insert. It happens BEFORE the supervisor
            // thread starts, so a `ready` racing this method's own return can never miss the
            // entry.
            lock(&self.registry).insert(
                id.clone(),
                Console {
                    seq,
                    provider,
                    session_key,
                    cwd: cwd.to_string(),
                    mode,
                    cols: size.cols,
                    rows: size.rows,
                    started_at_ms: now_ms(),
                    writer: Arc::new(Mutex::new(writer)),
                    master: master.clone(),
                    killer: Arc::new(Mutex::new(killer)),
                    scrollback: scrollback.clone(),
                    gate: gate.clone(),
                    readied: false,
                    running: true,
                    exit_code: None,
                },
            );
        }

        let (data_events, data_id) = (self.events.clone(), id.clone());
        let (exit_events, exit_id) = (self.events.clone(), id.clone());
        let exit_registry = self.registry.clone();
        thread::spawn(move || {
            supervise(
                child,
                reader,
                master,
                gate,
                move |chunk| {
                    // The ring first, then the wire. A reattach that raced an emit would otherwise
                    // replay a scrollback that was missing the very chunk it was about to be sent.
                    lock(&scrollback).push(chunk);
                    data_events.data(ConsoleData {
                        id: data_id.clone(),
                        data_b64: encode_b64(chunk),
                    });
                },
                move |code| {
                    {
                        // One mutation, then out — the emit below must not happen under the lock.
                        // The entry is absent when `close` won the race; the exit still goes out,
                        // so "exactly once per console" holds however the console ended.
                        let mut map = lock(&exit_registry);
                        if let Some(console) = map.get_mut(&exit_id) {
                            console.running = false;
                            console.exit_code = code;
                        }
                    }
                    exit_events.exit(ConsoleExit {
                        id: exit_id,
                        exit_code: code,
                    });
                },
            );
        });

        Ok(id)
    }

    /// Declare that a terminal is attached to this console and start delivering its output.
    ///
    /// The View calls this once, last in its attach order, after its `console://data` listener is
    /// registered and its terminal is mounted. Everything buffered before the call is delivered
    /// afterwards, in order — the pump reads the pty from the beginning, so readying late costs
    /// latency and never content.
    ///
    /// **Idempotent**, because the View has more than one honest reason to call it twice: React's
    /// StrictMode double-invokes mount effects, and a reattach after a reload legitimately re-runs
    /// the same path. Opening an open gate is a no-op.
    ///
    /// An unknown id is an error; an exited-but-present console is `Ok` and does nothing — its
    /// gate was opened during teardown regardless.
    pub fn ready(&self, id: &str) -> Result<(), ApiError> {
        // Opened with the registry lock released: `gate_to_open` returns having dropped it.
        if let Some(gate) = gate_to_open(&self.registry, id)? {
            gate.open();
        }
        Ok(())
    }

    /// Write base64-decoded bytes to a console's pty.
    ///
    /// **An unknown id is an error; an exited console is not.** They look similar and are not: a
    /// keystroke arriving for a console that has just exited is an ordinary race against the
    /// `console://exit` event still in flight, and rejecting it would put a spurious failure in
    /// front of the owner for something nothing did wrong. An id that was never in the registry is
    /// a caller bug, and saying so is how it gets found.
    pub fn input(&self, id: &str, data_b64: &str) -> Result<(), ApiError> {
        let bytes = decode_b64(data_b64)?;
        let writer = {
            let map = lock(&self.registry);
            match map.get(id) {
                None => return Err(unknown_console(id)),
                Some(console) if !console.running => return Ok(()),
                Some(console) => console.writer.clone(),
            }
        };
        // Registry lock released above, before any I/O — see the locking note at the top of the
        // file. This write is the one that can block for as long as the child refuses to read.
        let mut writer = lock(&writer);
        writer
            .write_all(&bytes)
            .map_err(|_| ApiError::host("could not write to the console").with_detail("id", id))?;
        writer
            .flush()
            .map_err(|_| ApiError::host("could not flush the console").with_detail("id", id))
    }

    /// Tell a console's pty its new size.
    ///
    /// A resize against an already-exited console is `Ok` and does nothing: the pty is closed and
    /// there is no size to set, but a fit-addon firing once more as its window tears down is not
    /// an error the owner should hear about.
    pub fn resize(&self, id: &str, cols: u32, rows: u32) -> Result<(), ApiError> {
        let size = pty_size(cols, rows);
        let master = {
            // One mutation: the stored size (so a reattach's summary says what the pty believes)
            // and the handle out, together, under one hold.
            let mut map = lock(&self.registry);
            match map.get_mut(id) {
                None => return Err(unknown_console(id)),
                Some(console) => {
                    console.cols = size.cols;
                    console.rows = size.rows;
                    console.master.clone()
                }
            }
        };
        // A nested hold around one `ioctl` — an atom in the locking rule's own sense, and on a
        // per-console mutex rather than the registry. Bound to a NAMED guard rather than matched
        // on a temporary: a temporary in tail position outlives the `Arc` it borrows from, which
        // the borrow checker refuses (and rightly — the guard would outlive the mutex it locks).
        let slot = lock(&master);
        let Some(pty) = slot.as_ref() else {
            return Ok(());
        };
        pty.resize(size)
            .map_err(|_| ApiError::host("could not resize the console").with_detail("id", id))
    }

    /// Kill a console's child and drop its registry entry.
    ///
    /// **An unknown id is `NOT_FOUND`, which is where this parts company with Studio** (whose
    /// `console_close` was silently idempotent). Pigeon's API contract states that unknown ids
    /// return `NOT_FOUND` for every console command, and a service that answered `Ok` would leave
    /// the command layer no way to honour it. Closing a console twice is therefore an error the
    /// second time — the View closes what it has just listed, and an exited console is still
    /// listed.
    ///
    /// No data is lost by killing: every engine records its session as it runs and stays resumable
    /// afterwards, which is what makes "close" a safe verb here at all.
    pub fn close(&self, id: &str) -> Result<(), ApiError> {
        // One removal, then out. The entry — and with it the writer, the master handle and the
        // killer — is dropped at the end of this method, outside the lock.
        let removed = { lock(&self.registry).remove(id) };
        let Some(console) = removed else {
            return Err(unknown_console(id));
        };
        // Nothing is emitted here. The supervisor thread owns the one `console://exit` this
        // console will ever produce: it fires when `wait()` returns, whether that is because of
        // this kill or because the child ended on its own. A close that emitted its own exit would
        // double it for a live console and invent one for a console that had already reported.
        kill_child(&console.killer);
        // **And the gate is opened, which on macOS is the half that actually works.** A child that
        // has already printed something and called `exit` is blocked in the tty drain and is past
        // the point where any signal reaches it; only a reader on the master releases it. Measured
        // 2026-09-13: without this line, closing a never-readied console left the child alive and
        // emitted no exit for at least 4 seconds. See [`PumpGate`].
        console.gate.open();
        Ok(())
    }

    /// Every console this host knows about, in the order they were opened.
    ///
    /// Two callers, both needing the same thing: a View that reloaded and has to reattach its
    /// terminals to consoles that never stopped running, and focus-or-open, which must not start a
    /// second engine on a session that already has one.
    pub fn list(&self) -> Vec<ConsoleSummaryDto> {
        list_rows(&self.registry)
    }

    /// [`ConsoleService::list`] in the shape the command returns, so the wrapper is one line and
    /// the DTO is named once.
    pub fn list_result(&self) -> ConsoleListResult {
        ConsoleListResult {
            consoles: self.list(),
        }
    }

    /// The tail of a console's recent output, base64-encoded, for a terminal that is
    /// (re)attaching.
    ///
    /// `truncated` says the reply is not the whole story — either the ring has evicted older bytes
    /// or `max_bytes` cut it — so the View can say so rather than implying the session began here.
    pub fn scrollback(
        &self,
        id: &str,
        max_bytes: Option<usize>,
    ) -> Result<ConsoleScrollbackDto, ApiError> {
        let scrollback = {
            let map = lock(&self.registry);
            match map.get(id) {
                None => return Err(unknown_console(id)),
                Some(console) => console.scrollback.clone(),
            }
        };
        // Copied and encoded with the registry released; only this console's own ring is held, and
        // only for the copy.
        let (bytes, truncated) = { lock(&scrollback).tail(max_bytes) };
        Ok(ConsoleScrollbackDto {
            id: id.to_string(),
            data_b64: encode_b64(&bytes),
            truncated,
        })
    }

    /// Attach a session key to a console that was started without one.
    ///
    /// A `new` console has no discoverable session until its engine writes a record, which can be
    /// seconds later. When the session list finds one whose working directory and start time
    /// match, this is how the console stops being anonymous — the mode still says `new`, because
    /// that is what happened, and the key says which session it became.
    ///
    /// Linking the same key twice is `Ok` (the discovery pass is allowed to run again); linking a
    /// DIFFERENT key is a conflict rather than an overwrite, because one of the two answers is
    /// wrong and silently taking the newer one would move a live terminal onto another session's
    /// row.
    pub fn link_session(&self, id: &str, key: SessionKey) -> Result<(), ApiError> {
        if !key.is_valid() {
            return Err(ApiError::invalid("a session key needs a usable id"));
        }
        let mut map = lock(&self.registry);
        let Some(console) = map.get_mut(id) else {
            return Err(unknown_console(id));
        };
        if key.provider_id != console.provider {
            return Err(ApiError::invalid(format!(
                "console {id} runs {}, not {}",
                console.provider.label(),
                key.provider_id.label()
            )));
        }
        match &console.session_key {
            Some(existing) if *existing == key => Ok(()),
            Some(existing) => Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!("console {id} is already attached to a session"),
            )
            .with_detail("sessionKeyId", existing.id())),
            None => {
                console.session_key = Some(key);
                Ok(())
            }
        }
    }

    /// The console currently hosting a session, if any.
    ///
    /// **A running console wins over an ended one.** The caller is deciding between focusing a
    /// terminal and resuming the session afresh, and a console whose child has exited answers the
    /// second question wrongly: it exists, but nothing is running in it. Among equals the most
    /// recently opened wins, because that is the one the owner was last looking at.
    pub fn find_by_session(&self, key: &SessionKey) -> Option<String> {
        let map = lock(&self.registry);
        let mut best: Option<(bool, u64, &String)> = None;
        for (id, console) in map.iter() {
            if console.session_key.as_ref() != Some(key) {
                continue;
            }
            let rank = (console.running, console.seq, id);
            if best.map(|b| rank > b).unwrap_or(true) {
                best = Some(rank);
            }
        }
        best.map(|(_, _, id)| id.clone())
    }

    /// Kill every hosted console. Called on the way out of the process: a CLI whose host has gone
    /// is an orphan holding a pty, and nothing else would ever reap it.
    pub fn close_all(&self) {
        // Clone the handles out under the lock; kill outside it. A kill is quick, but shutdown is
        // the last place to hold a process-wide lock across a syscall loop.
        let consoles: Vec<(KillerSlot, Arc<PumpGate>)> = {
            let mut map = lock(&self.registry);
            let handles = map
                .values()
                .map(|c| (c.killer.clone(), c.gate.clone()))
                .collect();
            map.clear();
            handles
        };
        for (killer, gate) in &consoles {
            kill_child(killer);
            // The same release `close` performs, and it matters most here: a console the owner
            // never attached to is exactly the one whose child is parked in the tty drain, and a
            // signal cannot reach it. Leaving it would orphan a CLI holding a pty after Pigeon is
            // gone.
            gate.open();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    // -- test doubles ----------------------------------------------------------------------------

    /// Every event a service emitted, in order, so ordering and exactly-once are assertions rather
    /// than claims.
    #[derive(Default)]
    struct Recorder {
        data: Mutex<Vec<ConsoleData>>,
        exits: Mutex<Vec<ConsoleExit>>,
    }

    impl Recorder {
        fn bytes(&self) -> Vec<u8> {
            lock(&self.data)
                .iter()
                .flat_map(|d| BASE64.decode(&d.data_b64).expect("emitted data is base64"))
                .collect()
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.bytes()).into_owned()
        }

        fn exits(&self) -> Vec<ConsoleExit> {
            lock(&self.exits).clone()
        }
    }

    impl ConsoleEvents for Recorder {
        fn data(&self, payload: ConsoleData) {
            lock(&self.data).push(payload);
        }

        fn exit(&self, payload: ConsoleExit) {
            lock(&self.exits).push(payload);
        }
    }

    /// Poll until `done`, or fail the run rather than hang it.
    fn wait_until(mut done: impl FnMut() -> bool, what: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            if done() {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pigeon-console-{tag}-{}-{}",
            std::process::id(),
            next_seq()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// Write a file into `dir` and make it executable, because [`is_executable_file`] means it.
    fn write_program(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&path, mode).expect("chmod");
        }
        path
    }

    // -- PATH resolution (ported) --------------------------------------------------------------

    #[test]
    fn an_exact_name_on_path_resolves_and_wins_over_an_extension() {
        let dir = scratch_dir("exact");
        write_program(&dir, "claude", "#!/bin/sh\n");
        write_program(&dir, "claude.exe", "native");

        let found = resolve_on_path("claude", Some(dir.as_os_str()), Some(OsStr::new(".EXE")))
            .expect("resolves");
        assert_eq!(found, dir.join("claude"));
    }

    #[test]
    fn a_pathext_extension_resolves_when_the_bare_name_is_absent() {
        let dir = scratch_dir("ext");
        write_program(&dir, "claude.cmd", "npm shim");

        let pathext = OsStr::new(".EXE;.CMD");
        let found = resolve_on_path("claude", Some(dir.as_os_str()), Some(pathext));
        if cfg!(windows) {
            // Compared case-insensitively on purpose: PATHEXT is conventionally uppercase while
            // the file on disk is not, and NTFS matches either way.
            let found = found.expect("resolves");
            assert_eq!(
                found.to_string_lossy().to_lowercase(),
                dir.join("claude.cmd").to_string_lossy().to_lowercase()
            );
        } else {
            // Off Windows there are no PATHEXT extensions to try, and a `.cmd` is not a program
            // name — the resolution correctly finds nothing.
            assert_eq!(found, None);
        }
    }

    /// The directories are tried in order, so the first PATH entry holding a hit wins.
    #[test]
    fn the_first_path_entry_holding_a_hit_wins() {
        let first = scratch_dir("first");
        let second = scratch_dir("second");
        write_program(&first, "codex", "a");
        write_program(&second, "codex", "b");

        let joined = std::env::join_paths([&first, &second]).expect("join");
        let found = resolve_on_path("codex", Some(joined.as_os_str()), None).expect("resolves");
        assert_eq!(found, first.join("codex"));
    }

    /// The failure the owner is most likely to hit, and it must be a named refusal rather than an
    /// OS error from four layers down.
    /// Measured 2026-09-23: a Pigeon started from a shell that a Claude Code session spawned
    /// carries `CLAUDE_CODE_CHILD_SESSION`, and every `claude` it resumed then printed "Transcript
    /// saving is off" and wrote nothing to `~/.claude` — so the session Pigeon had just resumed
    /// vanished from the very files Pigeon reads.
    #[test]
    fn a_console_child_never_inherits_a_parent_claude_sessions_markers() {
        let mut builder = CommandBuilder::new("claude");
        for name in pathenv::INHERITED_SESSION_MARKERS {
            builder.env(name, "inherited");
        }
        // The owner's own Claude configuration is not a marker and must survive.
        builder.env("CLAUDE_CODE_USE_BEDROCK", "1");

        apply_child_env(&mut builder, pathenv::child_env());

        for name in pathenv::INHERITED_SESSION_MARKERS {
            assert_eq!(builder.get_env(name), None, "{name} leaked into the child");
        }
        assert_eq!(
            builder.get_env("CLAUDE_CODE_USE_BEDROCK"),
            Some(OsStr::new("1"))
        );
        assert_eq!(
            builder.get_env("TERM"),
            Some(OsStr::new("xterm-256color")),
            "stripping must not cost the overrides"
        );
    }

    #[test]
    fn an_absent_program_resolves_to_nothing_rather_than_the_bare_name() {
        let dir = scratch_dir("absent");
        let exe_ext = Some(OsStr::new(".EXE"));
        assert_eq!(
            resolve_on_path("claude", Some(dir.as_os_str()), exe_ext),
            None
        );
        assert_eq!(resolve_on_path("claude", None, None), None);
    }

    /// A directory named `claude` is not a program named `claude`.
    #[test]
    fn a_directory_is_never_mistaken_for_the_executable() {
        let dir = scratch_dir("dir");
        std::fs::create_dir_all(dir.join("claude")).expect("mkdir");
        assert_eq!(resolve_on_path("claude", Some(dir.as_os_str()), None), None);
    }

    /// The unix half of the port: a file on PATH that nobody can execute is not the CLI. Studio's
    /// resolver stopped at `is_file()`, which on Windows is the whole question and on unix is not.
    #[cfg(unix)]
    #[test]
    fn a_file_without_the_execute_bit_is_not_the_engine_binary() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir("noexec");
        let path = dir.join("opencode");
        std::fs::write(&path, "notes, not a program").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        assert_eq!(
            resolve_on_path("opencode", Some(dir.as_os_str()), None),
            None
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(
            resolve_on_path("opencode", Some(dir.as_os_str()), None),
            Some(path)
        );
    }

    // -- argv ------------------------------------------------------------------------------------

    /// **Each engine's own resume invocation, which is the whole reason this is not Studio's
    /// hardcoded `--resume`.** A shape guessed from one engine starts nothing on another.
    #[test]
    fn each_provider_resumes_with_its_own_words() {
        let cases = [
            (
                ProviderId::ClaudeCode,
                vec!["--resume".to_string(), "s-1".to_string()],
            ),
            (
                ProviderId::Codex,
                vec!["resume".to_string(), "s-1".to_string()],
            ),
            (
                ProviderId::OpenCode,
                vec!["--session".to_string(), "s-1".to_string()],
            ),
        ];
        for (provider, expected) in cases {
            assert_eq!(
                launch_args(provider, ConsoleMode::Resume, Some("s-1")),
                expected
            );
            // And the argv the spawn actually receives: the resolved program, then those words.
            let exe = PathBuf::from("/usr/local/bin").join(provider.program());
            let args = launch_args(provider, ConsoleMode::Resume, Some("s-1"));
            let argv = launch_argv(&exe, &args, None);
            let mut want: Vec<OsString> = vec![exe.as_os_str().to_os_string()];
            want.extend(expected.iter().map(OsString::from));
            assert_eq!(argv, want);
        }
    }

    #[test]
    fn a_new_console_passes_the_cli_no_arguments_at_all() {
        for provider in ProviderId::ALL {
            assert!(launch_args(provider, ConsoleMode::New, None).is_empty());
            // Even handed an id: `new` means new, and an argv that resumed anyway would be the
            // mode quietly losing to a stale field.
            assert!(launch_args(provider, ConsoleMode::New, Some("s-1")).is_empty());
        }
    }

    #[test]
    fn a_blank_session_id_is_not_a_resume_target() {
        for blank in ["", "   ", "\t\n"] {
            let args = launch_args(ProviderId::ClaudeCode, ConsoleMode::Resume, Some(blank));
            assert!(args.is_empty());
        }
        assert_eq!(normalize_sid(Some("  s-1  ")).as_deref(), Some("s-1"));
        assert_eq!(normalize_sid(None), None);
    }

    #[test]
    fn sizes_are_clamped_into_what_a_pty_accepts() {
        let zero = pty_size(0, 0);
        assert_eq!((zero.cols, zero.rows), (MIN_DIM as u16, MIN_DIM as u16));
        let huge = pty_size(u32::MAX, u32::MAX);
        assert_eq!((huge.cols, huge.rows), (MAX_DIM as u16, MAX_DIM as u16));
        assert!(
            u32::from(huge.cols) <= i16::MAX as u32,
            "must stay inside Windows' COORD i16"
        );
        let ordinary = pty_size(120, 30);
        assert_eq!((ordinary.cols, ordinary.rows), (120, 30));
        assert_eq!((ordinary.pixel_width, ordinary.pixel_height), (0, 0));
    }

    // -- the pump (ported) -----------------------------------------------------------------------

    /// A torrent of output must not become one giant event: the whole chunk has to be encoded,
    /// bridged and decoded before any of it can be drawn.
    #[test]
    fn pty_output_arrives_in_bounded_chunks_and_loses_nothing() {
        let source: Vec<u8> = (0..(MAX_CHUNK * 3 + 17)).map(|i| (i % 251) as u8).collect();
        let chunks = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sink = chunks.clone();

        pump(
            Box::new(std::io::Cursor::new(source.clone())),
            move |chunk| {
                lock(&sink).push(chunk.to_vec());
            },
        );

        let chunks = lock(&chunks);
        assert!(chunks.len() > 1, "the fixture is bigger than one chunk");
        assert!(
            chunks.iter().all(|c| c.len() <= MAX_CHUNK),
            "a chunk exceeded the {MAX_CHUNK} byte bound"
        );
        assert_eq!(chunks.concat(), source, "output was reordered or dropped");
    }

    #[test]
    fn an_immediately_closed_pty_produces_no_chunks_at_all() {
        let mut calls = 0usize;
        pump(Box::new(std::io::Cursor::new(Vec::new())), |_| calls += 1);
        assert_eq!(calls, 0);
    }

    // -- registry lifecycle (ported) -------------------------------------------------------------

    #[derive(Debug, Default)]
    struct RecordingKiller {
        kills: Arc<AtomicBool>,
    }

    impl ChildKiller for RecordingKiller {
        fn kill(&mut self) -> std::io::Result<()> {
            self.kills.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(RecordingKiller {
                kills: self.kills.clone(),
            })
        }
    }

    /// A registry entry with no device behind it. `master: None` is the state a console reaches
    /// after it exits, which is exactly what makes the lifecycle testable without a pty.
    fn fake_console(
        seq: u64,
        provider: ProviderId,
        cwd: &str,
        key: Option<SessionKey>,
        killed: Arc<AtomicBool>,
    ) -> Console {
        Console {
            seq,
            provider,
            mode: if key.is_some() {
                ConsoleMode::Resume
            } else {
                ConsoleMode::New
            },
            session_key: key,
            cwd: cwd.to_string(),
            cols: 80,
            rows: 24,
            started_at_ms: 1_700_000_000_000,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            master: Arc::new(Mutex::new(None)),
            killer: Arc::new(Mutex::new(Box::new(RecordingKiller { kills: killed }))),
            scrollback: Arc::new(Mutex::new(Scrollback::default())),
            gate: Arc::new(PumpGate::default()),
            readied: false,
            running: true,
            exit_code: None,
        }
    }

    fn service_with(consoles: Vec<(&str, Console)>) -> (ConsoleService, Arc<Recorder>) {
        let events = Arc::new(Recorder::default());
        let service = ConsoleService::with_events(events.clone());
        for (id, console) in consoles {
            lock(&service.registry).insert(id.to_string(), console);
        }
        (service, events)
    }

    /// The whole lifecycle the contract promises: open -> list -> close, and what each state says.
    #[test]
    fn a_console_is_listed_until_it_is_closed_and_an_unknown_id_is_not_found() {
        let killed = Arc::new(AtomicBool::new(false));
        let key = SessionKey::new(ProviderId::ClaudeCode, "s-1");
        let (service, events) = service_with(vec![
            (
                "c1",
                fake_console(
                    1,
                    ProviderId::ClaudeCode,
                    "/a",
                    Some(key.clone()),
                    killed.clone(),
                ),
            ),
            (
                "c2",
                fake_console(
                    2,
                    ProviderId::Codex,
                    "/b",
                    None,
                    Arc::new(AtomicBool::new(false)),
                ),
            ),
        ]);

        let rows = service.list();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "c1");
        assert_eq!(rows[0].cwd, "/a");
        assert_eq!(rows[0].session_key.as_ref(), Some(&key));
        assert_eq!(rows[0].provider, ProviderId::ClaudeCode);
        assert_eq!(rows[0].mode, "resume");
        assert_eq!(
            rows[0].state, "starting",
            "a console that has not readied is not yet running"
        );
        assert_eq!(rows[0].exit_code, None);
        assert_eq!(rows[0].scrollback_bytes, 0);
        assert_eq!(rows[1].mode, "new", "a fresh console names no session");
        assert_eq!(rows[1].session_key, None);
        assert_eq!(rows[1].provider, ProviderId::Codex);

        // Ready flips starting -> running, and is idempotent.
        service.ready("c1").expect("a known console");
        service.ready("c1").expect("StrictMode calls it twice");
        assert_eq!(service.list()[0].state, "running");

        // Close: the child is killed, the entry goes, and nothing is emitted.
        service.close("c1").expect("c1 was open");
        assert!(
            killed.load(Ordering::SeqCst),
            "close must terminate the child"
        );
        assert_eq!(service.list().len(), 1);
        assert!(
            events.exits().is_empty(),
            "close is not an ending; the supervisor reports that"
        );

        // A second close, and an id that never existed, are both NOT_FOUND.
        for id in ["c1", "never-existed"] {
            let err = service
                .close(id)
                .expect_err("an unknown console is refused");
            assert_eq!(err.code, ApiErrorCode::NotFound);
        }
    }

    /// An exited console keeps its row, with its code, until it is closed — so a View that reloads
    /// still learns what happened rather than finding a gap.
    #[test]
    fn an_exited_console_keeps_its_row_with_the_exit_code() {
        let (service, _) = service_with(vec![(
            "c1",
            fake_console(
                1,
                ProviderId::OpenCode,
                "/a",
                None,
                Arc::new(AtomicBool::new(false)),
            ),
        )]);
        {
            let mut map = lock(&service.registry);
            let console = map.get_mut("c1").expect("present");
            console.running = false;
            console.exit_code = Some(130);
        }

        let rows = service.list();
        assert_eq!(rows.len(), 1, "an exited console is still listed");
        assert_eq!(rows[0].state, "exited");
        assert_eq!(rows[0].exit_code, Some(130));
    }

    /// Open order, not id order — `c10` must not sort between `c1` and `c2`.
    #[test]
    fn the_listing_is_in_open_order() {
        let (service, _) = service_with(
            [(1u64, "c1"), (2, "c2"), (10, "c10")]
                .into_iter()
                .map(|(seq, id)| {
                    (
                        id,
                        fake_console(
                            seq,
                            ProviderId::ClaudeCode,
                            "/a",
                            None,
                            Arc::new(AtomicBool::new(false)),
                        ),
                    )
                })
                .collect(),
        );
        let ids: Vec<String> = service.list().into_iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            vec!["c1".to_string(), "c2".to_string(), "c10".to_string()]
        );
    }

    #[test]
    fn console_ids_are_unique_within_the_process_and_carry_their_open_order() {
        let minted: Vec<(u64, String)> = (0..64).map(|_| mint_id()).collect();
        let unique: std::collections::HashSet<&String> = minted.iter().map(|(_, id)| id).collect();
        assert_eq!(unique.len(), minted.len());
        assert!(minted.iter().all(|(seq, id)| *id == format!("c{seq}")));
        assert!(
            minted.windows(2).all(|w| w[1].0 > w[0].0),
            "open order must be strictly increasing"
        );
    }

    /// A `new` console learns its session afterwards; a second, different answer is a conflict
    /// rather than a silent move onto another session's row.
    #[test]
    fn a_new_console_can_be_linked_to_the_session_its_engine_eventually_writes() {
        let (service, _) = service_with(vec![(
            "c1",
            fake_console(
                1,
                ProviderId::Codex,
                "/a",
                None,
                Arc::new(AtomicBool::new(false)),
            ),
        )]);
        let key = SessionKey::new(ProviderId::Codex, "s-9");

        assert_eq!(service.find_by_session(&key), None);
        service.link_session("c1", key.clone()).expect("links");
        assert_eq!(service.find_by_session(&key), Some("c1".to_string()));
        assert_eq!(service.list()[0].session_key.as_ref(), Some(&key));
        let mode = service.list()[0].mode.clone();
        assert_eq!(
            mode, "new",
            "the mode records what happened, not the outcome"
        );

        service
            .link_session("c1", key.clone())
            .expect("the same key twice is not a conflict");
        let err = service
            .link_session("c1", SessionKey::new(ProviderId::Codex, "s-other"))
            .expect_err("a different session is a conflict");
        assert_eq!(err.code, ApiErrorCode::Conflict);

        // A key from another engine is a caller bug, not a conflict.
        let err = service
            .link_session("c1", SessionKey::new(ProviderId::ClaudeCode, "s-9"))
            .expect_err("a provider mismatch is refused");
        assert_eq!(err.code, ApiErrorCode::InvalidArgument);
        assert_eq!(
            service.link_session("c9", key).unwrap_err().code,
            ApiErrorCode::NotFound
        );
    }

    /// A running console wins over an ended one: the caller is deciding whether to focus or
    /// resume.
    #[test]
    fn find_by_session_prefers_a_console_that_is_still_running() {
        let key = SessionKey::new(ProviderId::ClaudeCode, "s-1");
        let (service, _) = service_with(vec![
            (
                "c1",
                fake_console(
                    1,
                    ProviderId::ClaudeCode,
                    "/a",
                    Some(key.clone()),
                    Arc::new(AtomicBool::new(false)),
                ),
            ),
            (
                "c2",
                fake_console(
                    2,
                    ProviderId::ClaudeCode,
                    "/a",
                    Some(key.clone()),
                    Arc::new(AtomicBool::new(false)),
                ),
            ),
        ]);
        // Both running: the most recently opened is the one the owner was looking at.
        assert_eq!(service.find_by_session(&key), Some("c2".to_string()));

        lock(&service.registry)
            .get_mut("c2")
            .expect("present")
            .running = false;
        assert_eq!(
            service.find_by_session(&key),
            Some("c1".to_string()),
            "an ended console answers 'is one already running' wrongly"
        );
        assert_eq!(
            service.find_by_session(&SessionKey::new(ProviderId::Codex, "s-1")),
            None
        );
    }

    // -- the attach gate (ported) ----------------------------------------------------------------

    /// A child whose ending is under the test's control, so [`supervise`] can be driven end to end
    /// without a pty. `wait()` parks on a latch until the test (or `kill`) releases it — which is
    /// what a real CLI suspended on Windows' cursor handshake is doing, and therefore the state
    /// the gate has to be correct in.
    #[derive(Debug)]
    struct ScriptedChild {
        code: u32,
        release: Arc<PumpGate>,
    }

    impl ChildKiller for ScriptedChild {
        fn kill(&mut self) -> std::io::Result<()> {
            self.release.open();
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(ScriptedChild {
                code: self.code,
                release: self.release.clone(),
            })
        }
    }

    impl Child for ScriptedChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(None)
        }

        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            self.release.wait();
            Ok(portable_pty::ExitStatus::with_exit_code(self.code))
        }

        fn process_id(&self) -> Option<u32> {
            None
        }

        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }

    /// Start a [`supervise`] over a fixed byte source and a scripted child. Returns the pump gate,
    /// the child's release latch, the accumulated output, and the exit channel.
    #[allow(clippy::type_complexity)]
    fn scripted_console(
        source: Vec<u8>,
    ) -> (
        Arc<PumpGate>,
        Arc<PumpGate>,
        Arc<Mutex<Vec<u8>>>,
        mpsc::Receiver<Option<i32>>,
    ) {
        let gate = Arc::new(PumpGate::default());
        let release = Arc::new(PumpGate::default());
        let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (tx, rx) = mpsc::channel::<Option<i32>>();

        let child: Box<dyn Child + Send + Sync> = Box::new(ScriptedChild {
            code: 0,
            release: release.clone(),
        });
        let sink = seen.clone();
        let pump_gate = gate.clone();
        thread::spawn(move || {
            supervise(
                child,
                Box::new(std::io::Cursor::new(source)),
                Arc::new(Mutex::new(None)),
                pump_gate,
                move |chunk| lock(&sink).extend_from_slice(chunk),
                move |code| {
                    let _ = tx.send(code);
                },
            );
        });

        (gate, release, seen, rx)
    }

    /// The wedge the gate exists for: nothing leaves the pty until a terminal says it is
    /// listening, and everything that was waiting then arrives, in order.
    ///
    /// The fixture opens with the exact four bytes ConPTY opens with, because those are the bytes
    /// whose premature delivery caused the wedge on Windows.
    #[test]
    fn output_is_withheld_until_ready_and_then_arrives_in_order() {
        let source = b"\x1b[6nfirst-line\r\nsecond-line\r\n".to_vec();
        let (gate, release, seen, rx) = scripted_console(source.clone());

        // A real interval, not an instant: an emptiness assertion made too quickly would pass
        // merely because the pump had not been scheduled yet, which proves nothing.
        thread::sleep(Duration::from_millis(150));
        assert!(
            lock(&seen).is_empty(),
            "output must stay in the pty until ready — got {:?}",
            String::from_utf8_lossy(&lock(&seen))
        );
        assert!(
            rx.try_recv().is_err(),
            "an unready console must not have ended"
        );

        gate.open();
        wait_until(
            || lock(&seen).len() == source.len(),
            "the buffered output to flow",
        );
        assert_eq!(
            *lock(&seen),
            source,
            "buffered output must arrive whole and in order"
        );

        // And the console still ends normally afterwards.
        release.open();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(20))
                .expect("the console must report an ending"),
            Some(0)
        );
    }

    /// A console closed before it was ever readied must still tear down. Without the unconditional
    /// `gate.open()` in [`supervise`]'s teardown, the pump would stay parked on a signal nobody
    /// can send any more and this would hang instead of failing.
    #[test]
    fn a_console_closed_before_it_was_ever_ready_still_ends() {
        let (_gate, release, _seen, rx) = scripted_console(b"never delivered".to_vec());

        // close: the child is killed, and `ready` is never called.
        release.open();

        assert_eq!(
            rx.recv_timeout(Duration::from_secs(20))
                .expect("an unready console must still end when it is closed"),
            Some(0)
        );
    }

    /// The three answers the ready handshake can give, against the real registry function.
    #[test]
    fn ready_is_idempotent_and_names_an_unknown_console() {
        let console = fake_console(
            1,
            ProviderId::ClaudeCode,
            "/a",
            None,
            Arc::new(AtomicBool::new(false)),
        );
        let gate = console.gate.clone();
        let (service, _) = service_with(vec![("c1", console)]);

        // Unknown id -> a named error, never a silent success.
        let err = service
            .ready("c9")
            .expect_err("an unknown console must be refused");
        assert_eq!(err.code, ApiErrorCode::NotFound);
        assert!(
            err.message.contains("c9"),
            "the message must name the id: {}",
            err.message
        );

        assert!(!gate.is_open(), "a console starts un-readied");

        // Idempotent: StrictMode's doubled mount effect and a reattach both call this more than
        // once.
        for _ in 0..3 {
            service.ready("c1").expect("a known console");
            assert!(gate.is_open());
        }

        // Exited but still listed -> Ok, with nothing to open.
        lock(&service.registry)
            .get_mut("c1")
            .expect("present")
            .running = false;
        assert!(
            gate_to_open(&service.registry, "c1")
                .expect("an exited console is still known")
                .is_none(),
            "an exited console must not hand back a gate to open"
        );
    }

    // -- bounded, UTF-8-safe scrollback ----------------------------------------------------------

    #[test]
    fn scrollback_is_bounded_and_never_cuts_a_multi_byte_character_in_half() {
        // A three-byte character, repeated. Nothing here is ASCII, so any byte-aligned cut that
        // ignored UTF-8 would land mid-character about two times in three.
        const CHAR: &str = "あ";
        assert_eq!(CHAR.len(), 3);
        let mut ring = Scrollback::default();

        // Push past the cap, in chunks that do not divide it: the eviction point is then
        // guaranteed to fall inside a character rather than politely between two.
        let chunk = CHAR.repeat(701); // 2103 bytes, coprime with nothing in particular
        let mut pushed = 0usize;
        while pushed < SCROLLBACK_CAP * 2 {
            ring.push(chunk.as_bytes());
            pushed += chunk.len();
        }

        let (bytes, truncated) = ring.tail(None);
        assert!(truncated, "a ring that has evicted must say so");
        assert!(
            bytes.len() <= SCROLLBACK_CAP,
            "the ring is bounded at {SCROLLBACK_CAP}"
        );
        assert!(
            bytes.len() > SCROLLBACK_CAP - 4,
            "and it is bounded, not tiny"
        );
        let text = std::str::from_utf8(&bytes).expect("the tail must be valid UTF-8");
        assert!(
            text.chars().all(|c| c == 'あ'),
            "a cut character would show as something else"
        );

        // Every max_bytes that lands mid-character: 3 offsets x every sequence position.
        for cut in 1..=12usize {
            let (bytes, truncated) = ring.tail(Some(SCROLLBACK_CAP / 2 + cut));
            assert!(truncated);
            let text = std::str::from_utf8(&bytes).expect("a mid-character cut must be trimmed");
            assert!(text.starts_with(CHAR));
            // Forward, never backward: the reply is never longer than what was asked for.
            assert!(bytes.len() <= SCROLLBACK_CAP / 2 + cut);
        }

        // A chunk larger than the whole ring keeps its tail, and keeps it whole.
        let mut ring = Scrollback::default();
        ring.push(CHAR.repeat(SCROLLBACK_CAP).as_bytes());
        let (bytes, truncated) = ring.tail(None);
        assert!(truncated);
        assert!(bytes.len() <= SCROLLBACK_CAP);
        assert!(std::str::from_utf8(&bytes).is_ok());
    }

    #[test]
    fn a_short_scrollback_is_returned_whole_and_says_it_was_not_truncated() {
        let mut ring = Scrollback::default();
        ring.push(b"$ ls\r\n");
        ring.push("naïve — ✓\r\n".as_bytes());
        let (bytes, truncated) = ring.tail(None);
        assert!(!truncated, "nothing was left out");
        assert_eq!(String::from_utf8_lossy(&bytes), "$ ls\r\nnaïve — ✓\r\n");
        assert_eq!(ring.len(), bytes.len());
    }

    #[test]
    fn scrollback_names_an_unknown_console_rather_than_returning_an_empty_reply() {
        let (service, _) = service_with(vec![(
            "c1",
            fake_console(
                1,
                ProviderId::ClaudeCode,
                "/a",
                None,
                Arc::new(AtomicBool::new(false)),
            ),
        )]);
        assert_eq!(
            service.scrollback("c9", None).unwrap_err().code,
            ApiErrorCode::NotFound
        );
        let empty = service.scrollback("c1", None).expect("a known console");
        assert_eq!(empty.data_b64, "");
        assert!(!empty.truncated);
    }

    // -- the real-PTY smoke test -----------------------------------------------------------------

    /// **One genuine pseudo-terminal, through the whole service: resolve, spawn, gate, pump,
    /// exit.**
    ///
    /// This is the test the port exists to pass. Studio's console.rs had never executed a single
    /// line on macOS — it compiled and was never run — so everything below (that `openpty` works
    /// outside a terminal, that a child spawned into it produces bytes, that `wait()` returns its
    /// code, that the EIO-as-EOF path ends the pump, that the exit arrives exactly once) was
    /// unproven on this platform until this test ran.
    ///
    /// **A scratch shell script, never a real engine.** A test that started `claude` would spend
    /// the owner's quota, depend on their login state and write a transcript into their history.
    /// What is under test is this module's plumbing, and any child at all exercises it — so the
    /// fixture is a program named `claude` on a PATH of the test's own making, which also proves
    /// [`resolve_on_path`] and the PATH override are wired to the real spawn.
    #[test]
    fn a_real_pty_streams_output_and_reports_the_exit_code_exactly_once() {
        const MARKER: &str = "pigeon-pty-ok";
        let dir = scratch_dir("smoke");
        // `exit 3`: a code no runtime produces by accident, so "the code came from the child" is
        // not an inference. stderr is redirected into stdout because a pty is one stream either
        // way.
        write_program(
            &dir,
            "claude",
            &format!("#!/bin/sh\nprintf '%s\\n' '{MARKER}'\nexit 3\n"),
        );

        let events = Arc::new(Recorder::default());
        let service = ConsoleService::with_events(events.clone()).with_path_var(dir.clone().into());

        let id = service
            .open(
                ProviderId::ClaudeCode,
                None,
                &dir.to_string_lossy(),
                120,
                30,
                ConsoleMode::New,
            )
            .expect("a real pty must open and spawn on this machine");

        // The gate, against a real device: the child has been spawned and may already have run,
        // and not one byte may reach the View before it says it is listening.
        thread::sleep(Duration::from_millis(150));
        assert!(events.bytes().is_empty(), "not one byte before ready");
        assert!(events.exits().is_empty(), "and no exit either");
        assert_eq!(service.list()[0].state, "starting");

        service.ready(&id).expect("the console is known");

        wait_until(
            || !events.exits().is_empty(),
            "the console to report an ending",
        );
        let exits = events.exits();
        assert_eq!(
            exits.len(),
            1,
            "exactly one exit per console, however it ended"
        );
        assert_eq!(exits[0].id, id);
        assert_eq!(
            exits[0].exit_code,
            Some(3),
            "the child's own code, not a substitute"
        );

        // The output arrived, and it arrived BEFORE the exit — the ordering the contract promises.
        let text = events.text();
        assert!(
            text.contains(MARKER),
            "the pty produced no recognisable output (got {text:?})"
        );

        // And the same bytes are in the ring, ready for a reattach.
        let replay = service.scrollback(&id, None).expect("a known console");
        let bytes = BASE64.decode(&replay.data_b64).expect("base64");
        assert!(String::from_utf8_lossy(&bytes).contains(MARKER));
        assert!(!replay.truncated);

        // The row survives the ending, with the code on it.
        let row = &service.list()[0];
        assert_eq!(row.state, "exited");
        assert_eq!(row.exit_code, Some(3));
        assert!(row.scrollback_bytes >= MARKER.len());

        // **Closing an already-ended console emits nothing.** The supervisor owns the one exit
        // this console will ever produce; a close that reported its own would double it.
        service.close(&id).expect("closing a listed console");
        thread::sleep(Duration::from_millis(150));
        assert_eq!(events.exits().len(), 1, "close must not emit a second exit");
        assert!(service.list().is_empty());
    }

    /// **A console the owner never attached to must still die when it is closed.** The macOS
    /// regression this file exists to have found: a child that printed a line and called `exit` is
    /// parked in the kernel's tty drain, past the reach of the SIGHUP `close` sends, and only a
    /// reader on the master releases it. Measured before [`ConsoleService::close`] opened the
    /// gate: no exit event in 4 seconds and a live child. With it, the ending arrives at once.
    ///
    /// The code is not asserted, only its arrival: whether `wait()` reports the child's own `exit
    /// 3` or the hangup depends on which of the two wins a race that has no wrong answer. What
    /// must be true is that the console ends, exactly once, without anyone ever having attached a
    /// terminal.
    #[test]
    fn a_real_console_that_was_never_readied_still_dies_when_it_is_closed() {
        let dir = scratch_dir("unreadied");
        write_program(&dir, "claude", "#!/bin/sh\nprintf 'chatty\\n'\nexit 3\n");

        let events = Arc::new(Recorder::default());
        let service = ConsoleService::with_events(events.clone()).with_path_var(dir.clone().into());
        let id = service
            .open(
                ProviderId::ClaudeCode,
                None,
                &dir.to_string_lossy(),
                80,
                24,
                ConsoleMode::New,
            )
            .expect("opens");

        thread::sleep(Duration::from_millis(200));
        assert!(
            events.exits().is_empty(),
            "an unattached console has not ended on its own"
        );
        assert!(
            events.bytes().is_empty(),
            "and its output is still in the terminal"
        );

        service.close(&id).expect("closing a listed console");
        wait_until(
            || !events.exits().is_empty(),
            "a closed console to report its ending",
        );
        assert_eq!(
            events.exits().len(),
            1,
            "exactly one exit, however the console ended"
        );
        assert!(service.list().is_empty());
    }

    /// Input reaches the child through the real pty, and the child's answer comes back.
    ///
    /// The half of the device the smoke test above cannot reach: `take_writer`, a blocking write,
    /// and a child that is actually reading its terminal. It also pins the ordering that matters
    /// for a reattach — the bytes are in the ring before they are on the wire.
    #[test]
    fn typing_into_a_real_pty_reaches_the_child_and_its_answer_comes_back() {
        let dir = scratch_dir("input");
        // Reads one line and echoes it back with a prefix, then exits 0.
        write_program(
            &dir,
            "codex",
            "#!/bin/sh\nread line\nprintf 'got:%s\\n' \"$line\"\n",
        );

        let events = Arc::new(Recorder::default());
        let service = ConsoleService::with_events(events.clone()).with_path_var(dir.clone().into());
        let id = service
            .open(
                ProviderId::Codex,
                None,
                &dir.to_string_lossy(),
                80,
                24,
                ConsoleMode::New,
            )
            .expect("opens");
        service.ready(&id).expect("known");

        service
            .input(&id, &encode_b64(b"hello-pigeon\n"))
            .expect("the write must reach the pty");
        wait_until(
            || events.text().contains("got:hello-pigeon"),
            "the child's answer",
        );

        let exit = wait_until_exit(&events);
        assert_eq!(exit.exit_code, Some(0));
        // An unknown console is still refused, even though this one is alive.
        assert_eq!(
            service.input("c-nope", &encode_b64(b"x")).unwrap_err().code,
            ApiErrorCode::NotFound
        );
        // Malformed base64 is refused rather than silently dropped.
        let bad = service.input(&id, "not base64!!").unwrap_err();
        assert_eq!(bad.code, ApiErrorCode::InvalidArgument);
        service.close(&id).expect("closes");
    }

    fn wait_until_exit(events: &Arc<Recorder>) -> ConsoleExit {
        wait_until(|| !events.exits().is_empty(), "an exit");
        events.exits().remove(0)
    }

    /// A resize against a live pty is a real `ioctl`, and the registry remembers the size a
    /// reattaching terminal will be told about.
    #[test]
    fn resizing_a_live_console_reaches_the_device_and_is_remembered() {
        let dir = scratch_dir("resize");
        // Sleeps long enough to still be alive for the resize, then ends on its own.
        write_program(&dir, "opencode", "#!/bin/sh\nsleep 5\n");

        let events = Arc::new(Recorder::default());
        let service = ConsoleService::with_events(events.clone()).with_path_var(dir.clone().into());
        let id = service
            .open(
                ProviderId::OpenCode,
                None,
                &dir.to_string_lossy(),
                80,
                24,
                ConsoleMode::New,
            )
            .expect("opens");
        service.ready(&id).expect("known");

        service
            .resize(&id, 132, 43)
            .expect("the device must accept a resize");
        let row = &service.list()[0];
        assert_eq!((row.cols, row.rows), (132, 43));

        // Clamped on the way in, at the device as well as in the row.
        service
            .resize(&id, 0, 0)
            .expect("a zero-size fit is clamped, not refused");
        assert_eq!((service.list()[0].cols, service.list()[0].rows), (1, 1));
        assert_eq!(
            service.resize("c-nope", 80, 24).unwrap_err().code,
            ApiErrorCode::NotFound
        );

        service.close(&id).expect("closes");
    }

    /// The refusals that must happen before anything is spawned, and the one that happens because
    /// the engine is genuinely not installed.
    #[test]
    fn open_refuses_an_incoherent_request_before_it_opens_a_device() {
        let dir = scratch_dir("refusals");
        let events = Arc::new(Recorder::default());
        let service = ConsoleService::with_events(events.clone()).with_path_var(dir.clone().into());
        let cwd = dir.to_string_lossy().to_string();
        let key = SessionKey::new(ProviderId::ClaudeCode, "s-1");

        let cases: Vec<(&str, ApiErrorCode, ApiError)> = vec![
            (
                "a resume with no session",
                ApiErrorCode::InvalidArgument,
                service
                    .open(
                        ProviderId::ClaudeCode,
                        None,
                        &cwd,
                        80,
                        24,
                        ConsoleMode::Resume,
                    )
                    .unwrap_err(),
            ),
            (
                "a new console carrying a session",
                ApiErrorCode::InvalidArgument,
                service
                    .open(
                        ProviderId::ClaudeCode,
                        Some(key.clone()),
                        &cwd,
                        80,
                        24,
                        ConsoleMode::New,
                    )
                    .unwrap_err(),
            ),
            (
                "another engine's session",
                ApiErrorCode::InvalidArgument,
                service
                    .open(
                        ProviderId::Codex,
                        Some(key.clone()),
                        &cwd,
                        80,
                        24,
                        ConsoleMode::Resume,
                    )
                    .unwrap_err(),
            ),
            (
                "a blank session id",
                ApiErrorCode::InvalidArgument,
                service
                    .open(
                        ProviderId::ClaudeCode,
                        Some(SessionKey::new(ProviderId::ClaudeCode, "  ")),
                        &cwd,
                        80,
                        24,
                        ConsoleMode::Resume,
                    )
                    .unwrap_err(),
            ),
            (
                "a cwd that is not a directory",
                ApiErrorCode::InvalidArgument,
                service
                    .open(
                        ProviderId::ClaudeCode,
                        Some(key.clone()),
                        &dir.join("nope").to_string_lossy(),
                        80,
                        24,
                        ConsoleMode::Resume,
                    )
                    .unwrap_err(),
            ),
            (
                "an engine that is not installed",
                ApiErrorCode::HostFailure,
                service
                    .open(
                        ProviderId::ClaudeCode,
                        Some(key),
                        &cwd,
                        80,
                        24,
                        ConsoleMode::Resume,
                    )
                    .unwrap_err(),
            ),
        ];
        for (what, code, err) in cases {
            assert_eq!(err.code, code, "{what}: {}", err.message);
        }
        assert!(
            service.list().is_empty(),
            "a refused open must leave no console behind"
        );

        // And the mode word itself: an unknown one is refused rather than defaulted.
        assert_eq!(
            ConsoleMode::parse("resume").expect("known"),
            ConsoleMode::Resume
        );
        assert_eq!(ConsoleMode::parse("new").expect("known"), ConsoleMode::New);
        assert_eq!(
            ConsoleMode::parse("RESUME").unwrap_err().code,
            ApiErrorCode::InvalidArgument
        );
    }

    /// Shutdown kills every child and empties the registry, because a CLI whose host has gone is
    /// an orphan holding a pty that nothing else will ever reap.
    #[test]
    fn closing_the_app_terminates_every_hosted_console() {
        let killed: Vec<Arc<AtomicBool>> =
            (0..3).map(|_| Arc::new(AtomicBool::new(false))).collect();
        let (service, _) = service_with(
            killed
                .iter()
                .enumerate()
                .map(|(i, flag)| {
                    let id: &'static str = ["c1", "c2", "c3"][i];
                    (
                        id,
                        fake_console(i as u64 + 1, ProviderId::Codex, "/a", None, flag.clone()),
                    )
                })
                .collect(),
        );

        service.close_all();
        assert!(
            killed.iter().all(|k| k.load(Ordering::SeqCst)),
            "every child must be terminated"
        );
        assert!(service.list().is_empty());
        // Idempotent: both routes out of the process may call it.
        service.close_all();
    }
}
