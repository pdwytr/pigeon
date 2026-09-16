//! Pigeon — a one-view, multi-engine coding-agent session console.
//!
//! Pigeon reads what Claude Code, Codex and OpenCode already write on this machine, shows every
//! session in one list with its cost and its live status, and resumes any of them in an embedded
//! terminal. It copies Demo Studio's proven counting rules and PTY runtime; it imports
//! nothing from it.
//!
//! Four rules carry over from Studio and are load-bearing here:
//!
//! 1. **Read-only on sources.** Capture never mutates, moves or locks a file an engine owns.
//! 2. **Fail loud on unknown shapes.** Engine formats drift. An adapter that meets a record it
//!    does not recognise says so; it never renders a number derived from a guessed schema.
//! 3. **The counting rules are frozen.** Dedupe API calls by `message.id`; within one id take the
//!    per-key elementwise MAX, because `output_tokens` is a streaming counter.
//! 4. **These numbers are owner diagnostics, never agent targets.**

pub mod adapters;
pub mod api;
pub mod app_state;
pub mod cache;
pub mod domain;
pub mod pathenv;
pub mod services;
pub mod settings;
pub mod util;
pub mod wiring;

use std::sync::Arc;
use std::time::Duration;

use tauri::{Manager, PhysicalPosition};

use crate::api::events;

use crate::api::types::{MetricStateDto, Scope};
use crate::app_state::AppState;
use crate::services::console::ConsoleService;

/// Build and run the desktop app.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let status = Arc::new(wiring::status_service());
            let state = Arc::new(AppState::new(
                wiring::adapters(),
                wiring::metric_source(),
                Arc::new(wiring::HostStatus(Arc::clone(&status))),
                ConsoleService::new(app.handle().clone()),
                settings::default_path(),
            ));
            app.manage(Arc::clone(&state));
            spawn_worker(app.handle().clone(), state);
            dock_hover_window(app);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            api::commands::host_info,
            api::commands::settings_get,
            api::commands::settings_set,
            api::commands::sessions_list,
            api::commands::session_metrics,
            api::commands::projects_summary,
            api::commands::status_snapshot,
            api::commands::account_status,
            api::commands::folder_open,
            api::commands::project_pick,
            api::commands::console_open,
            api::commands::session_start,
            api::commands::console_ready,
            api::commands::console_input,
            api::commands::console_resize,
            api::commands::console_close,
            api::commands::console_list,
            api::commands::console_scrollback,
            api::commands::session_stop,
            api::commands::hover_toggle,
            api::commands::hover_select,
            api::commands::codex_hooks_status,
            api::commands::codex_hooks_enable,
            api::commands::codex_hooks_disable,
        ])
        .on_window_event(|window, event| {
            #[cfg(target_os = "macos")]
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    // macOS keeps the app alive while the hover window exists. Hide the main
                    // window instead of destroying it, so Dock reopen can show this same window.
                    api.prevent_close();
                    let _ = window.hide();
                    return;
                }
            }

            // On platforms where closing the main window ends the app, every console it owns goes
            // with it. A pty left running after its window is gone is a process the owner can no
            // longer see, reach, or stop from here.
            if let tauri::WindowEvent::Destroyed = event {
                if window.label() == "main" {
                    if let Some(state) = window.app_handle().try_state::<Arc<AppState>>() {
                        state.console.close_all();
                    }
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building pigeon")
        .run(|app: &tauri::AppHandle, event: tauri::RunEvent| {
            #[cfg(not(target_os = "macos"))]
            let _ = (&app, &event);

            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(window) = app.get_webview_window("hover") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        });
}

/// The hover is a corner instrument, not a centered utility window. Position it after the native
/// monitor is known and only then reveal it, so startup never flashes in the middle of the screen.
fn dock_hover_window(app: &mut tauri::App) {
    let Some(window) = app.get_webview_window("hover") else {
        return;
    };
    let Ok(Some(monitor)) = window.current_monitor() else {
        let _ = window.show();
        return;
    };
    let margin = 16;
    let x = monitor.position().x + margin;
    let y = monitor.position().y + margin;
    let _ = window.set_position(PhysicalPosition::new(x, y));
    let _ = window.show();
}

/// The one background worker: live status, then a top-up of whatever metrics are still pending.
///
/// It emits only when something changed. A status snapshot that is byte-identical to the last one
/// produces no event, because a View that re-renders every five seconds forever is a View that
/// loses the owner's scroll position and text selection for nothing.
fn spawn_worker(app: tauri::AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        // The SIGNATURE, not the snapshot. Comparing whole snapshots compares their clocks, which
        // always differ, so the worker would notify on every tick — see `StatusSnapshot::signature`.
        let mut last_live: Option<Vec<crate::domain::LiveSignature>> = None;
        let mut last_accounts: std::collections::BTreeMap<crate::domain::ProviderId, String> =
            std::collections::BTreeMap::new();
        loop {
            // Re-read each pass, so a change takes effect without a restart. This was a hardcoded
            // five seconds while the setting was stored, validated, round-tripped and ignored.
            // One status refresh costs about 0.1 s of subprocess time, so the 5 s default is
            // roughly 2% of a core. `settings` clamps the value to 1..=300 before it gets here.
            let seconds = state.settings().poll_interval_seconds.max(1);
            let interval = Duration::from_secs(seconds as u64);
            tokio::time::sleep(interval).await;

            let worker_state = Arc::clone(&state);
            // Everything goes inside the blocking task, discovery included. Awaiting the walk out
            // here ran a filesystem crawl on a runtime worker, and a panic in it escaped this task
            // and killed the worker for the rest of the session — no status, no metric fills, and
            // nothing on screen to say so.
            let outcome = tauri::async_runtime::spawn_blocking(move || {
                let inventory = worker_state.sessions.discover_now();
                let snapshot = worker_state.live().snapshot;
                let filled = worker_state.metrics.fill_pending(&inventory.sessions);
                (inventory, snapshot, filled)
            })
            .await;

            let (inventory, snapshot, filled) = match outcome {
                Ok(parts) => parts,
                Err(err) => {
                    // A panic in a fold or a probe used to be swallowed and retried in silence
                    // forever. Say it; the loop still continues, because one bad transcript must
                    // not end status for the whole session.
                    eprintln!("pigeon: a background pass did not finish ({err})");
                    continue;
                }
            };

            let signature = snapshot.signature();
            if last_live.as_ref() != Some(&signature) {
                let dto =
                    crate::api::types::StatusSnapshotDto::project(&snapshot, |key| match inventory
                        .get(key)
                    {
                        Some(session) => (
                            Some(session.project.leaf()),
                            Some(session.title.clone()),
                            session.name.clone(),
                        ),
                        None => (None, None, None),
                    });
                let _ = tauri::Emitter::emit(&app, events::STATUS_CHANGED, dto);
                // A change in what is live changes which sessions are in which scope.
                let _ = tauri::Emitter::emit(
                    &app,
                    events::SESSIONS_CHANGED,
                    events::SessionsChanged {
                        scope: Scope::Live,
                        generated_at_ms: snapshot.generated_at_ms,
                    },
                );
                last_live = Some(signature);
            }

            // Accounts, on the same pass. Identity and capacity are cached at five and fifteen
            // minutes, so this is a map lookup almost every time and a real read rarely — and
            // `capacity://changed` was declared in the contract, asserted by a test, and emitted
            // by nothing, which left the account strip unable to refresh at all.
            for account in state.accounts.all(false).await {
                let dto = crate::api::types::AccountStatusDto::from(&account);
                let fingerprint = account_fingerprint(&dto);
                let provider = account.provider;
                if last_accounts.get(&provider) != Some(&fingerprint) {
                    last_accounts.insert(provider, fingerprint);
                    let _ = tauri::Emitter::emit(
                        &app,
                        events::CAPACITY_CHANGED,
                        events::CapacityChanged {
                            provider,
                            account: dto,
                            generated_at_ms: util::now_ms(),
                        },
                    );
                }
            }

            // Batched, because a hundred rows landing as a hundred events is a hundred renders.
            for batch in filled.chunks(events::METRICS_BATCH) {
                let payload = events::SessionsMetrics {
                    rows: batch
                        .iter()
                        .map(|(key, state)| events::MetricsRow {
                            key: key.clone(),
                            metrics: MetricStateDto::from(state),
                        })
                        .collect(),
                    generated_at_ms: util::now_ms(),
                };
                let _ = tauri::Emitter::emit(&app, events::SESSIONS_METRICS, payload);
            }
        }
    });
}

/// What an account looks like, for change detection.
///
/// `readAtMs` moves on every read whether or not anything changed, so comparing whole accounts
/// would emit `capacity://changed` on every pass — the same mistake the status signature exists to
/// avoid. This is the part the owner can see: who is signed in, the plan, and each window's
/// percentage and reset.
fn account_fingerprint(dto: &crate::api::types::AccountStatusDto) -> String {
    let mut out = format!(
        "{}|{}|{:?}|{:?}|{}",
        dto.identity.signed_in,
        dto.capacity.supported,
        dto.identity.label,
        dto.capacity.plan,
        dto.capacity.stale
    );
    for window in &dto.capacity.windows {
        out.push_str(&format!(
            "|{:?}:{:.2}:{:?}",
            window.name, window.used_pct, window.resets_at_ms
        ));
    }
    if let Some(reached) = &dto.capacity.reached_limit {
        out.push_str(&format!("|reached:{reached}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::AccountStatusDto;
    use crate::domain::{
        AccountStatus, Capacity, CapacityWindow, CapacityWindowName, Identity, ProviderId,
    };

    fn account(used: f64, read_at: i64) -> AccountStatusDto {
        let mut identity = Identity::absent(ProviderId::ClaudeCode, read_at, None);
        identity.signed_in = true;
        identity.label = Some("someone@example.com".into());
        AccountStatusDto::from(&AccountStatus {
            provider: ProviderId::ClaudeCode,
            identity,
            capacity: Capacity {
                supported: true,
                windows: vec![CapacityWindow {
                    name: CapacityWindowName::FiveHour,
                    window_minutes: 300,
                    used_pct: used,
                    resets_at_ms: Some(9),
                }],
                ..Capacity::unsupported(ProviderId::ClaudeCode, read_at)
            },
        })
    }

    #[test]
    fn a_reread_that_changed_nothing_does_not_look_like_a_change() {
        // `readAtMs` moves every pass. Comparing whole accounts would emit on every tick, which is
        // the same trap `StatusSnapshot::signature` exists to avoid.
        assert_eq!(
            account_fingerprint(&account(42.0, 1)),
            account_fingerprint(&account(42.0, 900))
        );
    }

    #[test]
    fn a_moved_percentage_is_a_change() {
        assert_ne!(
            account_fingerprint(&account(42.0, 1)),
            account_fingerprint(&account(43.0, 1))
        );
    }
}
