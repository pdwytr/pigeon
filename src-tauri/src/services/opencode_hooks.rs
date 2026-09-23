//! OpenCode's in-process bridge: the two signals the database cannot carry.
//!
//! **Why this file exists.** OpenCode persists neither of the things Pigeon needs to be honest
//! about a live OpenCode, both measured on this Mac 2026-09-18 against `opencode` 1.18.31:
//!
//! 1. **A pending permission ask is written nowhere.** During a real `external_directory` ask that
//!    held the turn for the whole wait, the `permission` table held **zero** rows (it is the
//!    *saved rules* table, `/api/permission/saved`), the `event` stream carried **no** `permission`
//!    part (the tool part went `pending → running` and sat there, indistinguishable from a tool
//!    that is executing), and the only trace was a `log/opencode.log` line naming a `run` id and
//!    not a session id — and one run can own two sessions. So an ask read `Running`.
//!
//! 2. **A live process cannot be tied to a session.** A process holds no per-session file, socket
//!    or port — two live processes in one folder held only the shared `opencode.db`, `-wal`, `-shm`
//!    and the log. Attribution had to fall back to "the newest session in the cwd", so two live
//!    processes in one folder both resolved to the same session and collapsed into one row, and the
//!    second session was never observed at all.
//!
//! Both are visible **in-process**: the bus emits `permission.asked` / `permission.replied` with
//! `properties.sessionID`, and a plugin runs *inside* the process, so `process.pid` is its own.
//!
//! **The ruling, mirroring the Codex hooks decision (`0002-codex-hooks.md`).** Pigeon installs a
//! small OpenCode plugin that appends those facts to Pigeon's own JSONL. It is consented,
//! reversible, and it never touches `~/.local/share/opencode` — the read-only invariant. The plugin
//! file is written under the OpenCode config dir (`~/.config/opencode/plugins/`), which is the
//! owner's config rather than the engine's data, and it is removed by `uninstall`.
//!
//! **What this can and cannot see.** The bridge covers every permission and every session claim the
//! OpenCode UI would produce, in every client that loads the config. It requires an OpenCode restart
//! to take effect, because plugins are loaded at startup. A question tool is a separate event
//! (`question.asked`) and is already read from the part stream; it is not duplicated here.

use std::path::{Path, PathBuf};

use crate::api::errors::{EngineError, ErrorKind};
use crate::domain::ProviderId;

/// The plugin file's name. Named so an owner reading the directory knows whose it is.
const PLUGIN_FILE: &str = "pigeon-bridge.js";

/// The marker that identifies Pigeon's plugin. `is_installed` reads it back rather than trusting a
/// filename an unrelated plugin could share.
const PLUGIN_MARKER: &str = "pigeon-bridge";

/// Where the plugin appends. The reader lives in the adapter, because the adapter is what turns the
/// file into a state; this is only the path, and it is defined once, there.
pub fn events_path() -> PathBuf {
    crate::adapters::opencode::bridge_events_path()
}

/// Where OpenCode looks for plugins. `OPENCODE_CONFIG_DIR` wins, then `XDG_CONFIG_HOME`, then
/// `~/.config/opencode` — the same order OpenCode itself resolves, which is *not* the
/// `dirs::config_dir()` Pigeon's own settings use (on macOS that is `~/Library/Application
/// Support`, and OpenCode does not read it).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("OPENCODE_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("opencode");
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("opencode")
}

/// The plugin file's full path.
pub fn plugin_path() -> PathBuf {
    config_dir().join("plugins").join(PLUGIN_FILE)
}

/// The plugin source, with the event path baked in.
///
/// The path is embedded as a JSON string literal so a config dir containing a quote or a backslash
/// cannot break out of the literal and change the code.
pub fn plugin_source(events: &Path) -> String {
    let literal = serde_json::to_string(&events.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"// {marker} — installed by Pigeon. Safe to delete; Pigeon re-offers it.
//
// OpenCode persists neither a pending permission ask nor a process->session link, so Pigeon cannot
// see either from the database. This plugin runs inside the process, so it can, and appends both to
// Pigeon's own file. It never blocks, never replies, and swallows its own errors so it cannot
// affect OpenCode.
import {{ appendFileSync }} from "node:fs";

const EVENTS = {literal};
// This process's own pid and start time. `pid` alone is not enough: macOS reuses pids, and a new
// opencode that inherited a dead one's pid must not be shown as the dead one's session.
const PID = process.pid;
const STARTED = Math.round(Date.now() - process.uptime() * 1000);

function record(fields) {{
  try {{
    appendFileSync(EVENTS, JSON.stringify({{ pid: PID, started: STARTED, ...fields }}) + "\n");
  }} catch {{
    /* a bridge that breaks OpenCode is worse than one that misses a record */
  }}
}}

export const PigeonBridge = async () => ({{
  event: async ({{ event }}) => {{
    const type = event?.type;
    const p = event?.properties ?? {{}};
    if (!p.sessionID) return;
    if (type === "permission.asked" || type === "permission.replied") {{
      record({{
        kind: "permission",
        session_id: p.sessionID,
        event_name: type,
        request_id: p.id ?? p.requestID ?? "",
      }});
      return;
    }}
    // Any event that names a session is a claim: this process is working on it. The last one wins,
    // which is what makes the newest session the process's own rather than the folder's.
    if (type === "session.updated" || type === "session.created" || type === "message.updated") {{
      record({{ kind: "session", session_id: p.sessionID }});
    }}
  }},
}});
"#,
        marker = PLUGIN_MARKER,
        literal = literal,
    )
}

// ---------------------------------------------------------------------------------------------
// Installing the plugin
// ---------------------------------------------------------------------------------------------

/// What one install attempt did, for the View to report honestly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallReport {
    pub installed: bool,
    /// One sentence for the owner. Never a path we were not already showing.
    pub message: String,
}

/// Is Pigeon's plugin already there? A read of the owner's config, never a write.
///
/// Installed means Pigeon's plugin appending to *this* build's event file. A plugin left by the
/// pre-rename build still writes to `com.intanalytic.feather/`, which Pigeon no longer reads, so
/// it must be offered again rather than trusted. The path is matched as the JSON literal
/// [`plugin_source`] embeds.
pub fn is_installed() -> bool {
    is_installed_at(&config_dir(), &events_path())
}

fn is_installed_at(config: &Path, events: &Path) -> bool {
    let literal = serde_json::to_string(&events.to_string_lossy().into_owned()).unwrap_or_default();
    std::fs::read_to_string(config.join("plugins").join(PLUGIN_FILE))
        .map(|text| text.contains(PLUGIN_MARKER) && text.contains(&literal))
        .unwrap_or(false)
}

/// Write the plugin. Idempotent: a second install overwrites with the current event path, which is
/// what an owner who moved their config dir needs.
pub fn install() -> Result<InstallReport, EngineError> {
    install_at(&config_dir(), &events_path())
}

/// [`install`], with both paths given. The seam exists so a test can install into a temp config dir
/// and a temp event file without touching the owner's real OpenCode.
pub fn install_at(config: &Path, events: &Path) -> Result<InstallReport, EngineError> {
    let engine = ProviderId::OpenCode;
    let events_dir = events
        .parent()
        .ok_or_else(|| EngineError::of(engine, ErrorKind::Path))?;
    std::fs::create_dir_all(events_dir).map_err(|_| EngineError::of(engine, ErrorKind::Io))?;
    let plugins = config.join("plugins");
    std::fs::create_dir_all(&plugins).map_err(|_| EngineError::of(engine, ErrorKind::Io))?;
    std::fs::write(plugins.join(PLUGIN_FILE), plugin_source(events))
        .map_err(|_| EngineError::of(engine, ErrorKind::Io))?;

    // Read back rather than assume: the only state that fires is the file being there and being
    // Pigeon's.
    let installed = is_installed_at(config, events);
    Ok(InstallReport {
        installed,
        message: if installed {
            "Restart OpenCode to start showing when it waits on you.".to_string()
        } else {
            "The bridge was written but could not be read back.".to_string()
        },
    })
}

/// Remove the plugin. The event file is left alone: it is Pigeon's own record, and a stale line
/// cannot resurrect a session the adapter no longer sees live.
pub fn uninstall() -> Result<(), EngineError> {
    uninstall_at(&config_dir())
}

/// [`uninstall`], with the config dir given — see [`install_at`].
pub fn uninstall_at(config: &Path) -> Result<(), EngineError> {
    let path = config.join("plugins").join(PLUGIN_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        // Already gone is the state the caller asked for, not a failure.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(EngineError::of(ProviderId::OpenCode, ErrorKind::Io)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugin_embeds_the_event_path_as_a_json_literal() {
        let source = plugin_source(Path::new("/tmp/it's here/events.jsonl"));
        assert!(source.contains(PLUGIN_MARKER));
        assert!(
            source.contains(r#""/tmp/it's here/events.jsonl""#),
            "the path is a quoted literal: {source}"
        );
        // A backslash must be escaped, or the literal would end early.
        let escaped = plugin_source(Path::new(r"C:\Users\owner\events.jsonl"));
        assert!(escaped.contains(r#""C:\\Users\\owner\\events.jsonl""#));
    }

    #[test]
    fn the_plugin_source_records_permissions_and_session_claims_with_its_own_pid() {
        let source = plugin_source(Path::new("/tmp/events.jsonl"));
        assert!(source.contains("permission.asked"));
        assert!(source.contains("permission.replied"));
        assert!(source.contains("p.sessionID"));
        // The id field is `id` on the asked payload and `requestID` on the replied one.
        assert!(source.contains("p.id ?? p.requestID"));
        // The session claim, which is what makes attribution exact.
        assert!(source.contains(r#"kind: "session""#));
        assert!(source.contains("session.updated"));
        // Its own pid and start time, for the pid-reuse guard.
        assert!(source.contains("process.pid"));
        assert!(source.contains("process.uptime()"));
    }

    #[test]
    fn installing_writes_the_plugin_and_uninstalling_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("opencode");
        let events = dir.path().join("events.jsonl");

        assert!(
            !is_installed_at(&config, &events),
            "nothing is installed to start"
        );
        let report = install_at(&config, &events).expect("install runs");
        assert!(report.installed, "{}", report.message);
        assert!(is_installed_at(&config, &events));

        uninstall_at(&config).expect("uninstall runs");
        assert!(!is_installed_at(&config, &events));
        // Removing what is not there is the state asked for, not an error.
        uninstall_at(&config).expect("a second uninstall is fine");
    }

    #[test]
    fn a_foreign_file_of_the_same_name_is_not_mistaken_for_pigeons_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("opencode");
        let plugins = config.join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(plugins.join(PLUGIN_FILE), "// someone else's plugin").unwrap();
        let events = dir.path().join("events.jsonl");
        assert!(
            !is_installed_at(&config, &events),
            "the marker is what identifies Pigeon's plugin, not the filename"
        );
    }

    #[test]
    fn a_plugin_left_appending_to_another_apps_event_file_is_not_installed_so_it_is_offered_again()
    {
        // The feather -> pigeon rename moved the event file. A plugin written by the old build
        // still appends to the old path; reading it as installed would mean Pigeon never
        // re-offers it and OpenCode permission asks silently stop showing as waiting on you.
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("opencode");
        let stale = dir
            .path()
            .join("com.intanalytic.feather")
            .join("events.jsonl");
        let current = dir
            .path()
            .join("com.intanalytic.pigeon")
            .join("events.jsonl");
        install_at(&config, &stale).expect("install runs");
        assert!(!is_installed_at(&config, &current));

        // And re-accepting the offer points it at the current file.
        let report = install_at(&config, &current).expect("reinstall runs");
        assert!(report.installed, "{}", report.message);
    }
}
