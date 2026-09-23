//! Codex's hook events: the one signal that says Codex is blocked on the owner.
//!
//! **Why this file exists.** Codex's rollout records pair `task_started` with `task_complete` /
//! `turn_aborted` and nothing else. A Codex paused on an approval prompt still has an open
//! `task_started`, so the rollout tail alone reports `Running` for a session that is in fact
//! waiting on a human. That is a wrong signal of the worst kind: it tells the owner nothing needs
//! them.
//!
//! Codex 0.122+ ships hooks, and the `PermissionRequest` hook fires exactly when Codex stops to
//! ask. A hook is a command whose stdin is a JSON event; the payload carries the thread id, so an
//! event maps onto a session key with no guessing.
//!
//! **Pigeon never writes `~/.codex`.** Installing the hook is done by driving Codex's own
//! app-server (`config/batchWrite` + `hooks/list`), so Codex writes its own configuration. This
//! module reads the event file and decides what an event means; `codex_hook_install` performs the
//! install. Nothing here opens a file under `~/.codex` for writing.
//!
//! **What this can and cannot see.** Hooks cover shell and `apply_patch` approvals. Codex's
//! separate `request_user_input` overlay (a model question) has no hook event at all and cannot be
//! answered by one — the `updatedInput` field is reserved in Codex's own schema (openai/codex
//! #28969). A question therefore still reads `Running`; this file does not pretend otherwise.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crate::api::errors::{EngineError, ErrorKind};

/// The hook events Pigeon installs.
///
/// `PermissionRequest` is the point of the exercise. `PreToolUse` is what clears it: once the owner
/// answers and the tool actually runs, the approval is over. `Stop` closes the turn, so a denied
/// approval cannot leave a session pinned at "needs you" forever.
pub const INSTALLED_EVENTS: [&str; 3] = ["PermissionRequest", "PreToolUse", "Stop"];

/// How much of the event file is read. Events are one short JSON line each, appended forever, so
/// the tail is the only part that can describe the present; the whole file is never loaded.
const EVENT_TAIL_BYTES: u64 = 256 * 1024;

/// One hook firing, as much of it as Pigeon uses.
///
/// **There is no timestamp in the payload.** Codex sends the event name, the thread id, the cwd and
/// the transcript path, and nothing that orders two events. Order therefore comes from the file,
/// which is append-only: the last line for a session is the newest thing that happened to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookEvent {
    pub session_id: String,
    pub event_name: String,
}

/// The newest event per session, from the tail of the event file.
///
/// **The file is not one JSON object per line.** Codex hands the hook its payload on stdin with
/// no trailing newline, and the hook (`cat >>`) copies stdin verbatim, so successive events land
/// as a run of `{...}{...}` with nothing between them. Measured on this Mac 2026-09-17: the
/// installed hook had produced an 86 KB file holding 102 events and **zero** `\n`. Reading that
/// with `lines()` yields one unparseable line, so every event was dropped and a Codex parked on a
/// permission request still read `Running` — the exact lie this module exists to prevent. The
/// stream is therefore parsed as a stream of concatenated JSON values, which subsumes JSONL too.
///
/// Unreadable file, missing file, and a file of unrecognised records all answer the empty map.
/// That is not a claim about Codex: the caller only ever *overrides* a rollout reading with what
/// it finds here, so an empty map means "no hook evidence", never "nothing is running".
pub fn latest_by_session(path: &Path) -> BTreeMap<String, HookEvent> {
    let Ok(text) = read_tail(path, EVENT_TAIL_BYTES) else {
        return BTreeMap::new();
    };
    let mut out: BTreeMap<String, HookEvent> = BTreeMap::new();
    let mut rest = text.as_str();
    loop {
        let mut stream = serde_json::Deserializer::from_str(rest).into_iter::<serde_json::Value>();
        match stream.next() {
            Some(Ok(value)) => {
                let consumed = stream.byte_offset();
                // Last write wins: records are in append order, so a later one is a later fact.
                if let Some(event) = event_from_value(&value) {
                    out.insert(event.session_id.clone(), event);
                }
                // A successful parse always consumes bytes, so this cannot loop forever.
                rest = &rest[consumed..];
            }
            // The tail starts at an arbitrary offset, so the first record is often torn. So is a
            // record whose JSON is genuinely malformed. Either way, resynchronise on the next
            // `{` and keep going: the events behind a clipped one still describe the present. A
            // failed parse consumes at least one byte, so this terminates.
            Some(Err(_)) => {
                let consumed = stream.byte_offset().max(1);
                let from = consumed.min(rest.len());
                match rest[from..].find('{') {
                    Some(open) => rest = &rest[from + open..],
                    None => break,
                }
            }
            None => break,
        }
    }
    out
}

/// Where the installed hook appends. Beside Pigeon's own settings, never inside `~/.codex`.
pub fn events_path() -> PathBuf {
    crate::settings::default_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("codex-hook-events.jsonl")
}

/// One JSONL record. Unknown fields are ignored; the two fields this module needs are `session_id`
/// and `hook_event_name`. A line missing either is not an event and is skipped rather than guessed.
///
/// Only the tests call this: the live reader parses the concatenated stream, which needs the
/// deserializer rather than a per-line split. It stays as the smallest statement of "what a
/// record is" and is pinned against a payload captured from a real Codex run.
#[cfg(test)]
fn parse_line(line: &str) -> Option<HookEvent> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    event_from_value(&value)
}

/// One decoded hook payload. The two fields this module needs are `session_id` and
/// `hook_event_name`; a record missing either is not an event and is skipped rather than guessed.
fn event_from_value(value: &serde_json::Value) -> Option<HookEvent> {
    let session_id = value.get("session_id")?.as_str()?.trim();
    let event_name = value.get("hook_event_name")?.as_str()?.trim();
    if session_id.is_empty() || event_name.is_empty() {
        return None;
    }
    Some(HookEvent {
        session_id: session_id.to_string(),
        event_name: event_name.to_string(),
    })
}

/// Read at most `bytes` from the end of a file. Read-only, never locked: the file is Pigeon's own,
/// but the discipline is the same one the adapters follow.
fn read_tail(path: &Path, bytes: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(bytes)))?;
    let mut buf = Vec::with_capacity(bytes.min(len) as usize);
    file.take(bytes).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ---------------------------------------------------------------------------------------------
// Installing the hook
// ---------------------------------------------------------------------------------------------

/// What one install attempt did, for the View to report honestly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallReport {
    pub installed: bool,
    pub trusted: bool,
    /// One sentence for the owner. Never a path we were not already showing.
    pub message: String,
}

/// The `codex` binary this app-server is driven by, resolved the way every other engine call is.
fn codex_program() -> String {
    crate::domain::ProviderId::Codex.program().to_string()
}

/// The command the hook runs. `cat` copies the event JSON on stdin into the append-only file;
/// nothing is interpreted, so a payload Pigeon cannot parse can never corrupt the log.
///
/// Quoted because the path is derived from the platform config directory, which may contain spaces.
pub fn hook_command() -> String {
    hook_command_for(&events_path())
}

fn hook_command_for(events: &Path) -> String {
    format!("cat >> {}", shell_quote(&events.to_string_lossy()))
}

fn shell_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\\''"))
}

/// Is our hook already in the user's Codex config? A read of the owner's file, never a write.
pub fn is_installed() -> bool {
    is_installed_at(&codex_home())
}

fn is_installed_at(home: &Path) -> bool {
    std::fs::read_to_string(home.join("config.toml"))
        .map(|text| text.contains("codex-hook-events.jsonl"))
        .unwrap_or(false)
}

fn codex_home() -> PathBuf {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(home);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
}

/// Install the hook and trust it, by driving Codex's own app-server.
///
/// **Pigeon never edits `~/.codex/config.toml`.** Every change goes through Codex's
/// `config/batchWrite`, which is the same path Codex's own TUI uses to record hook trust, so
/// Codex parses, validates and writes its own file. Pigeon opens it for reading only.
///
/// Trust is the part that cannot be skipped: Codex silently ignores an untrusted hook, so an
/// install that wrote the hook and stopped would look successful and do nothing.
pub fn install() -> Result<InstallReport, EngineError> {
    install_at(&codex_home(), &events_path())
}

/// [`install`], with both paths given. The seam exists so a test can install into a temp Codex home
/// and a temp event file: `CODEX_HOME` in the environment is process-global and would leak into
/// every other test running beside this one.
pub fn install_at(home: &Path, events: &Path) -> Result<InstallReport, EngineError> {
    let dir = events
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| EngineError::of(crate::domain::ProviderId::Codex, ErrorKind::Path))?;
    std::fs::create_dir_all(&dir)
        .map_err(|_| EngineError::of(crate::domain::ProviderId::Codex, ErrorKind::Io))?;

    let mut server = AppServer::spawn(home)?;
    let command = hook_command_for(events);
    for event in INSTALLED_EVENTS {
        let value = serde_json::json!([{
            "matcher": ".*",
            "hooks": [{ "type": "command", "command": command }]
        }]);
        server.write_config(&format!("hooks.{event}"), "upsert", value)?;
    }

    // Trust has to follow the write, because the hash Codex reports is computed from the hook it
    // just parsed. Two round trips, in this order, and not one.
    let mut trusted = 0usize;
    for entry in server.hooks()? {
        if entry.command.as_deref() != Some(command.as_str()) {
            continue;
        }
        server.write_config(
            &format!("hooks.state.{}", quote_key_path(&entry.key)),
            "upsert",
            serde_json::json!({ "enabled": true, "trusted_hash": entry.current_hash }),
        )?;
        trusted += 1;
    }
    if trusted < INSTALLED_EVENTS.len() {
        return Ok(InstallReport {
            installed: false,
            trusted: false,
            message: format!(
                "Codex accepted {trusted} of {} hooks; nothing was trusted, so nothing will fire.",
                INSTALLED_EVENTS.len()
            ),
        });
    }

    // Read back rather than assume: "trusted" is the only state that actually fires.
    let confirmed = server
        .hooks()?
        .iter()
        .filter(|e| e.command.as_deref() == Some(command.as_str()))
        .all(|e| e.trust_status == "trusted");
    Ok(InstallReport {
        installed: true,
        trusted: confirmed,
        message: if confirmed {
            "Pigeon will now show when Codex is waiting on you.".to_string()
        } else {
            "The hooks were written but Codex has not trusted them yet.".to_string()
        },
    })
}

/// Remove the hook and its trust state. The event file is left alone: it is Pigeon's own record.
pub fn uninstall() -> Result<(), EngineError> {
    uninstall_at(&codex_home(), &events_path())
}

/// [`uninstall`], with both paths given — see [`install_at`].
pub fn uninstall_at(home: &Path, events: &Path) -> Result<(), EngineError> {
    let mut server = AppServer::spawn(home)?;
    let command = hook_command_for(events);
    for entry in server.hooks()? {
        if entry.command.as_deref() != Some(command.as_str()) {
            continue;
        }
        // Disabled, not deleted: Codex rejects a `null` TOML value, and an entry with
        // `enabled = false` is inert whether or not a hook still references it.
        server.write_config(
            &format!("hooks.state.{}", quote_key_path(&entry.key)),
            "replace",
            serde_json::json!({ "enabled": false, "trusted_hash": entry.current_hash }),
        )?;
    }
    for event in INSTALLED_EVENTS {
        server.write_config(&format!("hooks.{event}"), "replace", serde_json::json!([]))?;
    }
    Ok(())
}

/// A keyPath segment carrying a filesystem path: dots and slashes make it ambiguous unless quoted.
fn quote_key_path(segment: &str) -> String {
    format!("\"{}\"", segment.replace('\\', "\\\\").replace('"', "\\\""))
}

/// One hook as Codex reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookEntry {
    pub key: String,
    pub command: Option<String>,
    pub current_hash: String,
    pub trust_status: String,
}

/// A `codex app-server` driven over stdio for the few config calls the install needs.
///
/// Bounded and synchronous by construction: it is spawned for one install and dropped with it, and
/// never lives on the poll path. A reader thread turns the child's stdout into a channel so a
/// wedged Codex is a timeout rather than a hang.
struct AppServer {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: mpsc::Receiver<String>,
    next_id: i64,
}

const APP_SERVER_TIMEOUT: Duration = Duration::from_secs(20);

impl AppServer {
    fn spawn(home: &Path) -> Result<Self, EngineError> {
        use std::process::{Command, Stdio};
        let engine = crate::domain::ProviderId::Codex;
        let mut child = Command::new(codex_program())
            .args(["app-server", "--listen", "stdio://"])
            // Per-child, never the process environment: a test installing into a temp Codex home
            // must not redirect every other Codex call in this process at the same time.
            .env("CODEX_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| EngineError::of(engine, ErrorKind::NotInstalled))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| EngineError::of(engine, ErrorKind::Io))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::of(engine, ErrorKind::Io))?;
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            stdin,
            lines,
            next_id: 1,
        };
        server.initialize()?;
        Ok(server)
    }

    fn initialize(&mut self) -> Result<(), EngineError> {
        self.call(
            "initialize",
            serde_json::json!({
                "clientInfo": { "name": "pigeon", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        Ok(())
    }

    fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, EngineError> {
        use std::io::Write;
        let engine = crate::domain::ProviderId::Codex;
        let id = self.next_id;
        self.next_id += 1;
        let request = serde_json::json!({ "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{request}")
            .and_then(|_| self.stdin.flush())
            .map_err(|_| EngineError::of(engine, ErrorKind::Io))?;

        loop {
            let line = self
                .lines
                .recv_timeout(APP_SERVER_TIMEOUT)
                .map_err(|_| EngineError::of(engine, ErrorKind::Transport))?;
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if value.get("id").and_then(serde_json::Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                if std::env::var_os("PIGEON_DEBUG_APP_SERVER").is_some() {
                    eprintln!("app-server error for {method}: {error}");
                }
                return Err(EngineError::of(engine, ErrorKind::UnknownShape));
            }
            return Ok(value
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null));
        }
    }

    fn write_config(
        &mut self,
        key_path: &str,
        strategy: &str,
        value: serde_json::Value,
    ) -> Result<(), EngineError> {
        self.call(
            "config/batchWrite",
            serde_json::json!({
                "edits": [{ "keyPath": key_path, "mergeStrategy": strategy, "value": value }],
                "reloadUserConfig": false
            }),
        )?;
        Ok(())
    }

    /// Every hook Codex currently knows about, flattened across events.
    fn hooks(&mut self) -> Result<Vec<HookEntry>, EngineError> {
        let result = self.call("hooks/list", serde_json::json!({}))?;
        let mut out = Vec::new();
        for group in result
            .get("data")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            for hook in group
                .get("hooks")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                out.push(HookEntry {
                    key: hook
                        .get("key")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    command: hook
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    current_hash: hook
                        .get("currentHash")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    trust_status: hook
                        .get("trustStatus")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                });
            }
        }
        Ok(out)
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_events(dir: &Path, lines: &[&str]) -> PathBuf {
        let path = dir.join("codex-hook-events.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        path
    }

    #[test]
    fn the_last_event_for_a_session_wins_because_the_file_is_append_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_events(
            dir.path(),
            &[
                r#"{"session_id":"a","hook_event_name":"PermissionRequest"}"#,
                r#"{"session_id":"b","hook_event_name":"PermissionRequest"}"#,
                r#"{"session_id":"a","hook_event_name":"PreToolUse"}"#,
            ],
        );

        let events = latest_by_session(&path);
        assert_eq!(events["a"].event_name, "PreToolUse");
        assert_eq!(events["b"].event_name, "PermissionRequest");
    }

    #[test]
    fn a_malformed_or_incomplete_line_is_skipped_and_the_rest_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_events(
            dir.path(),
            &[
                "not json at all",
                r#"{"session_id":"a"}"#,
                r#"{"hook_event_name":"PermissionRequest"}"#,
                r#"{"session_id":"","hook_event_name":"PermissionRequest"}"#,
                r#"{"session_id":"a","hook_event_name":""}"#,
                r#"{"session_id":"a","hook_event_name":"PermissionRequest"}"#,
            ],
        );

        let events = latest_by_session(&path);
        assert_eq!(events.len(), 1);
        assert_eq!(events["a"].event_name, "PermissionRequest");
    }

    /// The file is a run of `{...}{...}` with no newlines, because Codex sends each payload on
    /// stdin with no trailing newline and the hook copies stdin verbatim. Measured on this Mac
    /// 2026-09-17: 102 events, zero `\n`. A `lines()` reader sees one unparseable line and loses
    /// every event, which is why a Codex on a permission request still read `Running`.
    #[test]
    fn concatenated_events_with_no_newlines_are_all_read_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let raw = concat!(
            r#"{"session_id":"b","hook_event_name":"PermissionRequest","tool_name":"Bash"}"#,
            r#"{"session_id":"a","hook_event_name":"PreToolUse"}"#,
            r#"{"session_id":"b","hook_event_name":"Stop"}"#,
        );
        assert_eq!(raw.matches('\n').count(), 0, "the fixture has no newlines");
        let path = dir.path().join("codex-hook-events.jsonl");
        std::fs::write(&path, raw).unwrap();

        let events = latest_by_session(&path);
        assert_eq!(events.len(), 2, "both sessions are seen: {events:?}");
        assert_eq!(events["a"].event_name, "PreToolUse");
        // Append order decides the winner: b's Stop supersedes its PermissionRequest.
        assert_eq!(events["b"].event_name, "Stop");
    }

    /// An event whose own payload contains braces and quoted braces must not split the stream.
    #[test]
    fn a_record_whose_payload_contains_braces_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        let raw = concat!(
            r#"{"session_id":"a","hook_event_name":"PermissionRequest","#,
            r#""tool_input":{"command":"echo '{ \"x\": 1 }' && printf '}'"}}"#,
            r#"{"session_id":"a","hook_event_name":"PreToolUse"}"#,
        );
        let path = dir.path().join("codex-hook-events.jsonl");
        std::fs::write(&path, raw).unwrap();

        let events = latest_by_session(&path);
        assert_eq!(events["a"].event_name, "PreToolUse");
    }

    /// The tail is read from an arbitrary offset, so its first record is often clipped. The
    /// records after it still describe the present and must not be lost with it.
    #[test]
    fn a_tail_that_begins_mid_record_still_reads_the_records_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let raw = concat!(
            r#""transcript_path":"/tmp/r.jsonl","hook_event_name":"Stop"}"#,
            r#"{"session_id":"a","hook_event_name":"PermissionRequest"}"#,
            r#"{"session_id":"b","hook_event_name":"Stop"}"#,
        );
        let path = dir.path().join("codex-hook-events.jsonl");
        std::fs::write(&path, raw).unwrap();

        let events = latest_by_session(&path);
        assert_eq!(events.len(), 2, "the clipped record hid nobody: {events:?}");
        assert_eq!(events["a"].event_name, "PermissionRequest");
        assert_eq!(events["b"].event_name, "Stop");
    }

    #[test]
    fn a_missing_event_file_is_an_empty_answer_not_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        assert!(latest_by_session(&dir.path().join("nothing-here.jsonl")).is_empty());
    }

    #[test]
    fn the_hook_command_quotes_a_path_that_may_contain_spaces() {
        assert_eq!(
            shell_quote("/Users/owner/Library/Application Support/x"),
            "'/Users/owner/Library/Application Support/x'"
        );
        // A quote in the path must not be able to end the quoting and start a new command.
        assert_eq!(shell_quote("/tmp/it's here"), "'/tmp/it'\\''s here'");
    }

    #[test]
    fn a_hook_key_with_dots_and_slashes_is_quoted_so_keypath_stays_unambiguous() {
        assert_eq!(
            quote_key_path("/Users/owner/.codex/config.toml:permission_request:0:0"),
            "\"/Users/owner/.codex/config.toml:permission_request:0:0\""
        );
    }

    /// The install, end to end, against the real `codex` binary in an isolated Codex home.
    ///
    /// **This is the test that matters.** Everything else here is pure parsing. This one proves
    /// the two things the feature stands on: that Codex accepts the hook shape Pigeon writes, and
    /// that Codex then reports it `trusted` — because an untrusted hook is silently skipped, and an
    /// install that stopped at "written" would look like success and do nothing.
    ///
    /// Skipped when `codex` is not on PATH, so a machine without it still builds and tests.
    #[test]
    fn installing_writes_a_hook_codex_accepts_and_trusts() {
        if std::process::Command::new("codex")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: codex is not on PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let events = dir.path().join("events.jsonl");

        let report = install_at(&home, &events).expect("install runs");
        assert!(report.installed, "{}", report.message);
        assert!(
            report.trusted,
            "Codex must trust the hook or it never fires: {}",
            report.message
        );

        // Codex wrote its own config; Pigeon only ever reads it back.
        let written = std::fs::read_to_string(home.join("config.toml")).expect("config written");
        for event in INSTALLED_EVENTS {
            assert!(
                written.contains(event),
                "the {event} hook is in Codex's config:\n{written}"
            );
        }
        assert!(
            written.contains("trusted_hash"),
            "trust was recorded:\n{written}"
        );

        uninstall_at(&home, &events).expect("uninstall runs");
    }

    #[test]
    fn the_hook_payload_codex_actually_sends_parses() {
        // Captured from a real Codex run during the POC. The two fields Pigeon reads are present
        // among several it ignores; if Codex ever renames one, this is the test that fails.
        let line = r#"{"session_id":"01a0abee-053f-7be2-9c8f-375f24327506","transcript_path":"/tmp/r.jsonl","cwd":"/tmp","hook_event_name":"SessionStart","model":"gpt-5.6-sol","permission_mode":"bypassPermissions","source":"startup"}"#;
        let event = parse_line(line).expect("a real Codex payload parses");
        assert_eq!(event.session_id, "01a0abee-053f-7be2-9c8f-375f24327506");
        assert_eq!(event.event_name, "SessionStart");
    }
}
