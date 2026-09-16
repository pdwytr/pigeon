//! Tauri commands.
//!
//! Every command here is thin: validate the arguments, call one service, project the result.
//! There is no provider parsing and no business rule in this file — those live in `adapters/`
//! and `services/`, which is what lets the engines change without the View noticing.
//!
//! **Every command that touches the filesystem, SQLite or a subprocess is `async` and does that
//! work inside `spawn_blocking`.** A synchronous command body runs on the macOS UI thread, so a
//! 14 MB transcript fold in one would freeze the window.

use std::collections::BTreeMap;
use std::sync::Arc;

use tauri::State;

use crate::api::errors::ApiError;
use crate::api::types::{
    AccountStatusDto, AccountStatusResult, AmbiguousProcessDto, CodexHooksReportDto,
    CodexHooksStatusDto, ConsoleListResult, ConsoleOpenedDto, ConsoleScrollbackDto, HostInfo,
    HoverVisibilityDto, MetricStateDto, PickedFolderDto, ProjectSummaryDto, ProjectSummaryResult,
    Scope, SessionDto, SessionListResult, SessionRowDto, StatusSnapshotDto, StopResultDto,
    StoppedProcessDto,
};
use crate::app_state::AppState;
use crate::domain::SessionKey;
use crate::services::console::ConsoleMode;
use crate::services::{projects, sessions::status_for};
use crate::settings::{Settings, SettingsPatch};
use crate::util::now_ms;

type Shared<'a> = State<'a, Arc<AppState>>;

/// The View's only platform signal.
#[tauri::command]
pub async fn host_info() -> HostInfo {
    HostInfo::detect()
}

#[tauri::command]
pub async fn settings_get(state: Shared<'_>) -> Result<Settings, ApiError> {
    Ok(state.settings())
}

#[tauri::command]
pub async fn settings_set(state: Shared<'_>, patch: SettingsPatch) -> Result<Settings, ApiError> {
    let (next, saved) = state.update_settings(|current| current.apply(patch));
    // The change IS applied in memory whatever the disk says, so the app behaves as asked for this
    // session. But a settings call that quietly fails to persist is how an owner loses the same
    // preference three times without ever being told, so a write failure is reported.
    if saved.is_err() {
        return Err(
            ApiError::host("the change was applied but could not be saved")
                .with_detail("recentWindowDays", next.recent_window_days.to_string()),
        );
    }
    Ok(next)
}

/// Every session in the requested scope, merged across the engines.
///
/// This never folds a transcript. Rows come back with `metrics: pending` and a background fill
/// emits them on `sessions://metrics` as they land.
#[tauri::command]
pub async fn sessions_list(
    state: Shared<'_>,
    scope: Scope,
    force: Option<bool>,
) -> Result<SessionListResult, ApiError> {
    let state = Arc::clone(&state);
    let force = force.unwrap_or(false);
    let inventory = state.sessions.inventory(force).await;
    let settings = state.settings();

    let result = tauri::async_runtime::spawn_blocking(move || {
        let picture = state.live();
        let cutoff = settings.recent_cutoff_ms(now_ms());
        let in_scope = state
            .sessions
            .in_scope(&inventory, scope, &picture.live, cutoff);

        let rows: Vec<SessionRowDto> = in_scope
            .iter()
            .map(|session| {
                let mut dto = SessionDto::from(*session);
                dto.metrics = MetricStateDto::from(&state.metrics.peek(session));
                dto.closed_at_ms = match scope {
                    // A live session has not closed. Reporting the fallback here would put a
                    // closed-at timestamp on a session that is running right now.
                    Scope::Live => None,
                    Scope::Recent => Some(state.sessions.closed_at(session)),
                };
                SessionRowDto {
                    session: dto,
                    status: status_for(&session.key, &picture.statuses, &picture.degraded),
                }
            })
            .collect();

        SessionListResult {
            scope,
            since_ms: match scope {
                Scope::Live => None,
                Scope::Recent => Some(cutoff),
            },
            rows,
            // Discovery's problems AND the status source's. An empty Live tab caused by a
            // status root we cannot read must not look like an idle machine.
            problems: inventory
                .problems
                .iter()
                .cloned()
                .chain(picture.problems.iter().cloned())
                .collect(),
            generated_at_ms: inventory.generated_at_ms,
        }
    })
    .await
    .map_err(|_| ApiError::host("the session scan did not finish"))?;

    Ok(result)
}

/// One session's counters, computed on demand if the background fill has not reached it.
#[tauri::command]
pub async fn session_metrics(
    state: Shared<'_>,
    key: SessionKey,
) -> Result<MetricStateDto, ApiError> {
    if !key.is_valid() {
        return Err(ApiError::invalid("a session id is required"));
    }
    let state = Arc::clone(&state);
    let mut inventory = state.sessions.inventory(false).await;
    if inventory.get(&key).is_none() {
        // The inventory can be ten seconds stale, and the View only ever asks for a key it got
        // from `sessions_list` — so a miss means the world moved between the two calls. One forced
        // re-walk is cheaper than telling the owner their session does not exist.
        inventory = state.sessions.inventory(true).await;
    }
    let state_for_blocking = Arc::clone(&state);
    let dto = tauri::async_runtime::spawn_blocking(move || {
        let Some(session) = inventory.get(&key) else {
            return Err(ApiError::not_found("no such session").with_detail("sid", key.sid.clone()));
        };
        Ok(MetricStateDto::from(
            &state_for_blocking.metrics.ensure(session),
        ))
    })
    .await
    .map_err(|_| ApiError::host("the count did not finish"))??;
    Ok(dto)
}

/// One card per working directory, over the requested scope.
#[tauri::command]
pub async fn projects_summary(
    state: Shared<'_>,
    scope: Scope,
) -> Result<ProjectSummaryResult, ApiError> {
    let state = Arc::clone(&state);
    let inventory = state.sessions.inventory(false).await;
    let settings = state.settings();

    let result = tauri::async_runtime::spawn_blocking(move || {
        let picture = state.live();
        let cutoff = settings.recent_cutoff_ms(now_ms());
        let in_scope = state
            .sessions
            .in_scope(&inventory, scope, &picture.live, cutoff);

        let summaries = projects::summarize_all(
            &in_scope,
            |session| state.metrics.peek(session),
            |key| status_for(key, &picture.statuses, &picture.degraded),
        );

        ProjectSummaryResult {
            scope,
            since_ms: match scope {
                Scope::Live => None,
                Scope::Recent => Some(cutoff),
            },
            projects: summaries.iter().map(ProjectSummaryDto::from).collect(),
            generated_at_ms: inventory.generated_at_ms,
        }
    })
    .await
    .map_err(|_| ApiError::host("the project rollup did not finish"))?;

    Ok(result)
}

/// The canonical live projection. Only sessions with an engine process are in it.
#[tauri::command]
pub async fn status_snapshot(state: Shared<'_>) -> Result<StatusSnapshotDto, ApiError> {
    let state = Arc::clone(&state);
    let inventory = state.sessions.inventory(false).await;
    let dto = tauri::async_runtime::spawn_blocking(move || {
        let snapshot = state.live().snapshot;
        StatusSnapshotDto::project(&snapshot, |key| match inventory.get(key) {
            Some(session) => (
                Some(session.project.leaf()),
                Some(session.title.clone()),
                session.name.clone(),
            ),
            // A live process whose session we have not discovered yet is still worth showing —
            // it is the most interesting row on the screen. It just has no title yet.
            None => (None, None, None),
        })
    })
    .await
    .map_err(|_| ApiError::host("the status scan did not finish"))?;
    Ok(dto)
}

/// Identity and capacity for every engine.
#[tauri::command]
pub async fn account_status(
    state: Shared<'_>,
    force: Option<bool>,
    provider: Option<crate::domain::ProviderId>,
) -> Result<AccountStatusResult, ApiError> {
    let force = force.unwrap_or(false);
    let mut accounts: BTreeMap<String, AccountStatusDto> = BTreeMap::new();
    match provider {
        Some(one) => {
            if let Some(status) = state.accounts.status(one, force).await {
                accounts.insert(one.as_str().to_string(), AccountStatusDto::from(&status));
            }
        }
        None => {
            for status in state.accounts.all(force).await {
                accounts.insert(
                    status.provider.as_str().to_string(),
                    AccountStatusDto::from(&status),
                );
            }
        }
    }
    Ok(AccountStatusResult {
        accounts,
        generated_at_ms: now_ms(),
    })
}

/// Open a folder in the host's file manager. Validated here so a View bug cannot ask the host
/// to open something that is not a directory.
#[tauri::command]
pub async fn folder_open(app: tauri::AppHandle, cwd: String) -> Result<(), ApiError> {
    let path = std::path::PathBuf::from(&cwd);
    if !path.is_dir() {
        return Err(ApiError::invalid("that is not a folder").with_detail("cwd", cwd));
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(path.to_string_lossy().to_string(), None::<&str>)
        .map_err(|_| ApiError::host("the folder could not be opened"))
}

/// The native folder picker. Returns `None` when the owner cancels — a cancel is not an error.
#[tauri::command]
pub async fn project_pick(app: tauri::AppHandle) -> Result<Option<PickedFolderDto>, ApiError> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog().file().pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    let picked = tauri::async_runtime::spawn_blocking(move || rx.recv().ok().flatten())
        .await
        .map_err(|_| ApiError::host("the picker did not return"))?;
    Ok(picked
        .and_then(|p| p.into_path().ok())
        .map(|p| PickedFolderDto {
            cwd: p.display().to_string(),
        }))
}

// ---------------------------------------------------------------------------------------------
// Consoles
// ---------------------------------------------------------------------------------------------

/// Resume a session in a pty. The console is created parked: nothing flows until `console_ready`.
#[tauri::command]
pub async fn console_open(
    state: Shared<'_>,
    session_key: SessionKey,
    cwd: String,
    cols: u32,
    rows: u32,
) -> Result<ConsoleOpenedDto, ApiError> {
    let state = Arc::clone(&state);
    let id = tauri::async_runtime::spawn_blocking(move || {
        state.console.open(
            session_key.provider_id,
            Some(session_key.clone()),
            &cwd,
            cols,
            rows,
            ConsoleMode::Resume,
        )
    })
    .await
    .map_err(|_| ApiError::host("the console did not open"))??;
    Ok(ConsoleOpenedDto { id })
}

/// Start a NEW session in a folder. This creates a console, not a pigeon session — the session
/// appears when the engine writes a discoverable record and discovery links the two.
#[tauri::command]
pub async fn session_start(
    state: Shared<'_>,
    provider: crate::domain::ProviderId,
    cwd: String,
    cols: u32,
    rows: u32,
) -> Result<ConsoleOpenedDto, ApiError> {
    let state = Arc::clone(&state);
    let id = tauri::async_runtime::spawn_blocking(move || {
        state
            .console
            .open(provider, None, &cwd, cols, rows, ConsoleMode::New)
    })
    .await
    .map_err(|_| ApiError::host("the console did not open"))??;
    Ok(ConsoleOpenedDto { id })
}

/// Let the output flow. Called last in the View's attach sequence, after its terminal exists.
#[tauri::command]
pub async fn console_ready(state: Shared<'_>, id: String) -> Result<(), ApiError> {
    state.console.ready(&id)
}

#[tauri::command]
pub async fn console_input(
    state: Shared<'_>,
    id: String,
    data_b64: String,
) -> Result<(), ApiError> {
    let state = Arc::clone(&state);
    // A pty write can block indefinitely when the child has stopped draining, so it never runs
    // on the UI thread.
    tauri::async_runtime::spawn_blocking(move || state.console.input(&id, &data_b64))
        .await
        .map_err(|_| ApiError::host("the keystroke did not reach the console"))?
}

#[tauri::command]
pub async fn console_resize(
    state: Shared<'_>,
    id: String,
    cols: u32,
    rows: u32,
) -> Result<(), ApiError> {
    state.console.resize(&id, cols, rows)
}

#[tauri::command]
pub async fn console_close(state: Shared<'_>, id: String) -> Result<(), ApiError> {
    let state = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || state.console.close(&id))
        .await
        .map_err(|_| ApiError::host("the console did not close"))?
}

#[tauri::command]
pub async fn console_list(state: Shared<'_>) -> Result<ConsoleListResult, ApiError> {
    Ok(state.console.list_result())
}

/// Bounded replay for a console the View is re-attaching to. Written before any live byte.
#[tauri::command]
pub async fn console_scrollback(
    state: Shared<'_>,
    id: String,
    max_bytes: Option<usize>,
) -> Result<ConsoleScrollbackDto, ApiError> {
    state.console.scrollback(&id, max_bytes)
}

// ---------------------------------------------------------------------------------------------
// Stopping a session
// ---------------------------------------------------------------------------------------------

/// Stop every process that can be *proven* to belong to this session, including ones pigeon did
/// not start. An ambiguous match stops nothing — a cwd match is not proof, because two sessions
/// can run in one folder.
#[tauri::command]
pub async fn session_stop(state: Shared<'_>, key: SessionKey) -> Result<StopResultDto, ApiError> {
    if !key.is_valid() {
        return Err(ApiError::invalid("a session id is required"));
    }
    let state = Arc::clone(&state);
    let for_blocking = key.clone();
    let outcome =
        tauri::async_runtime::spawn_blocking(move || state.status.stop_session(&for_blocking))
            .await
            .map_err(|_| ApiError::host("the stop did not finish"))?
            .map_err(ApiError::from)?;

    Ok(StopResultDto {
        key,
        stopped: outcome
            .stopped
            .iter()
            .map(|p| StoppedProcessDto {
                pid: p.pid,
                evidence: p.evidence.join("; "),
            })
            .collect(),
        already_stopped: outcome.already_stopped,
        ambiguous: outcome
            .ambiguous
            .iter()
            .map(|m| AmbiguousProcessDto {
                pid: m.pid,
                reason: m.why.clone(),
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------------------------
// The hover surface
// ---------------------------------------------------------------------------------------------

/// Show or hide the always-on-top live window, and remember which it was.
#[tauri::command]
pub async fn hover_toggle(
    app: tauri::AppHandle,
    state: Shared<'_>,
) -> Result<HoverVisibilityDto, ApiError> {
    use tauri::Manager;
    let window = app
        .get_webview_window("hover")
        .ok_or_else(|| ApiError::host("the hover window is not available"))?;
    let showing = !window.is_visible().unwrap_or(false);
    let result = if showing {
        window.show()
    } else {
        window.hide()
    };
    result.map_err(|_| ApiError::host("the hover window did not respond"))?;

    // Under one lock: this and a concurrent split save used to lose each other's change.
    let (_, saved) = state.update_settings(|mut current| {
        current.hover.visible = showing;
        current
    });
    let _ = saved;
    Ok(HoverVisibilityDto { visible: showing })
}

// ---------------------------------------------------------------------------------------------
// The Codex waiting-on-you hook
// ---------------------------------------------------------------------------------------------

/// Has the owner already let Pigeon see Codex waiting on them?
#[tauri::command]
pub async fn codex_hooks_status() -> CodexHooksStatusDto {
    CodexHooksStatusDto {
        installed: crate::services::codex_hooks::is_installed(),
    }
}

/// Install and trust the Codex hook. One owner decision, in the View, behind this command.
///
/// Blocking by construction — it spawns `codex app-server` and waits on it — so it runs on a
/// blocking thread rather than the UI thread. It builds no runtime, so `spawn_blocking` is safe
/// here for the reason the accounts service documents at length.
#[tauri::command]
pub async fn codex_hooks_enable() -> Result<CodexHooksReportDto, ApiError> {
    let report = tauri::async_runtime::spawn_blocking(crate::services::codex_hooks::install)
        .await
        .map_err(|_| ApiError::host("the Codex hook install did not finish"))?
        .map_err(|_| ApiError::host("Codex did not accept the hook"))?;
    Ok(CodexHooksReportDto {
        installed: report.installed,
        trusted: report.trusted,
        message: report.message,
    })
}

/// Undo the install: the hook is disabled and its definitions removed from Codex's config.
#[tauri::command]
pub async fn codex_hooks_disable() -> Result<(), ApiError> {
    tauri::async_runtime::spawn_blocking(crate::services::codex_hooks::uninstall)
        .await
        .map_err(|_| ApiError::host("the Codex hook removal did not finish"))?
        .map_err(|_| ApiError::host("Codex did not accept the removal"))
}

/// Bring the compact hover forward and tell it which row to select.
#[tauri::command]
pub async fn hover_select(app: tauri::AppHandle, key: SessionKey) -> Result<(), ApiError> {
    use tauri::{Emitter, Manager};
    if !key.is_valid() {
        return Err(ApiError::invalid("a session id is required"));
    }
    let hover = app
        .get_webview_window("hover")
        .ok_or_else(|| ApiError::host("the hover window is not available"))?;
    let _ = hover.show();
    let _ = hover.set_focus();
    app.emit(crate::api::events::SELECT_SESSION, key)
        .map_err(|_| ApiError::host("the selection did not reach the window"))
}
