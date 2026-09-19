// The Tauri implementation of `FeatherApi` — the ONLY module in the app that calls `invoke` or
// registers a host event listener.
//
// Everything here is a thin name/argument mapping. There is no caching, no merging and no
// interpretation: a command's answer is handed on exactly as Rust shaped it, because the moment
// this file starts reshaping a response it becomes a second, undocumented copy of the contract.
//
// `invoke` is reached through a shared dynamic import rather than a static one so that a vitest
// file importing any component does not drag `@tauri-apps/api` into its module graph, and so a
// browser build with no Tauri behind it can fall back to `fake.ts` without this module ever
// resolving (see `isTauri`).

import {
  type AccountStatusResult,
  type CodexHooksReport,
  type CodexHooksStatus,
  type ConsoleDataEvent,
  type ConsoleExitEvent,
  type ConsoleScrollback,
  type ConsoleSummary,
  EV_CONSOLE_DATA,
  EV_CONSOLE_EXIT,
  EV_SELECT_SESSION,
  EV_SESSIONS_CHANGED,
  EV_SESSIONS_METRICS,
  EV_STATUS_CHANGED,
  type HostInfo,
  type MetricState,
  type OpenCodeHooksReport,
  type OpenCodeHooksStatus,
  type ProjectSummaryResult,
  type ProviderId,
  type Scope,
  type SessionKey,
  type SessionListResult,
  type SessionsMetricsEvent,
  type Settings,
  type StatusSnapshot,
  type StopResult,
} from "../bindings";
import { subscribeHostEvent } from "../platform/hostEvents";
import type {
  ConsoleOpenArgs,
  FeatherApi,
  SessionStartArgs,
  SessionsChangedPayload,
  Unsubscribe,
} from "./types";

let coreApi: Promise<typeof import("@tauri-apps/api/core")> | null = null;

function core() {
  if (!coreApi) coreApi = import("@tauri-apps/api/core");
  return coreApi;
}

/**
 * Invoke one host command.
 *
 * **The payload is FLAT, and that is not a style choice.** Tauri v2 resolves each command
 * parameter by looking up its own name — lower-camel-cased from the Rust identifier — at the top
 * level of this object (`tauri::ipc::command::deserialize_json` does `value.get(key)`). An
 * `{ args: {...} }` envelope therefore hides every parameter, and the command is rejected with
 * "missing required key". This file shipped with that envelope and an audit found it: every
 * command taking a required argument failed, while the six taking none — and `account_status`,
 * whose two parameters are both `Option` and so tolerate a missing key — worked, which is exactly
 * why the app looked alive.
 *
 * `src/api/client.test.ts` pins the top-level key names against the Rust signatures.
 */
// `object`, not `Record<string, unknown>`: a declared interface has no index signature, so the
// typed arg objects in `api/types.ts` are not assignable to a Record even though their keys are
// exactly right. `object` accepts them and still refuses a primitive.
async function call<T>(command: string, args?: object): Promise<T> {
  const { invoke } = await core();
  return invoke<T>(command, args as Record<string, unknown> | undefined);
}

/** Is a Tauri host behind this webview? v2 stamps `__TAURI_INTERNALS__` onto the window before any
 *  app script runs, so this answers correctly on the first line of `main.tsx`. */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Adapt `hostEvents`' subscription — which is what handles the async-listen/sync-cleanup race —
 *  to the plain `Unsubscribe` the api interface exposes. */
function on<T>(event: string, cb: (payload: T) => void): Unsubscribe {
  const sub = subscribeHostEvent<T>(event, cb);
  return () => sub.stop();
}

export function createTauriApi(): FeatherApi {
  return {
    hostInfo: () => call<HostInfo>("host_info"),
    settingsGet: () => call<Settings>("settings_get"),
    settingsSet: (patch) => call<Settings>("settings_set", { patch }),
    sessionsList: (args) => call<SessionListResult>("sessions_list", args),
    projectsSummary: (args) => call<ProjectSummaryResult>("projects_summary", args),
    sessionMetrics: (args) => call<MetricState>("session_metrics", args),
    statusSnapshot: () => call<StatusSnapshot>("status_snapshot"),
    accountStatus: (args) => call<AccountStatusResult>("account_status", args ?? {}),
    projectPick: () => call<{ cwd: string } | null>("project_pick"),
    folderOpen: (args) => call<void>("folder_open", args),
    sessionStart: (args: SessionStartArgs) => call<{ id: string }>("session_start", args),
    sessionStop: (args) => call<StopResult>("session_stop", args),
    consoleOpen: (args: ConsoleOpenArgs) => call<{ id: string }>("console_open", args),
    consoleReady: (args) => call<void>("console_ready", args),
    consoleInput: (args) => call<void>("console_input", args),
    consoleResize: (args) => call<void>("console_resize", args),
    consoleClose: (args) => call<void>("console_close", args),
    consoleList: () => call<{ consoles: ConsoleSummary[] }>("console_list"),
    consoleScrollback: (args) => call<ConsoleScrollback>("console_scrollback", args),
    hoverToggle: () => call<{ visible: boolean }>("hover_toggle"),
    hoverSelect: (key) => call<void>("hover_select", { key }),
    codexHooksStatus: () => call<CodexHooksStatus>("codex_hooks_status"),
    codexHooksEnable: () => call<CodexHooksReport>("codex_hooks_enable"),
    codexHooksDisable: () => call<void>("codex_hooks_disable"),
    opencodeHooksStatus: () => call<OpenCodeHooksStatus>("opencode_hooks_status"),
    opencodeHooksEnable: () => call<OpenCodeHooksReport>("opencode_hooks_enable"),
    opencodeHooksDisable: () => call<void>("opencode_hooks_disable"),

    onSessionsChanged: (cb) => on<SessionsChangedPayload>(EV_SESSIONS_CHANGED, cb),
    onSessionsMetrics: (cb) => on<SessionsMetricsEvent>(EV_SESSIONS_METRICS, cb),
    onStatusChanged: (cb) => on<StatusSnapshot>(EV_STATUS_CHANGED, cb),
    onConsoleData: (cb) => on<ConsoleDataEvent>(EV_CONSOLE_DATA, cb),
    onConsoleExit: (cb) => on<ConsoleExitEvent>(EV_CONSOLE_EXIT, cb),
    onSelectSession: (cb) => on<SessionKey>(EV_SELECT_SESSION, cb),
  };
}

/** Re-exported so `main.tsx` can name the provider ids it offers without importing bindings twice. */
export type { ProviderId, Scope };
