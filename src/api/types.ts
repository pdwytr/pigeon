// The shape of the host as React sees it.
//
// `PigeonApi` is `docs/contracts/components.md` §4 verbatim, plus the event subscriptions — the
// contract says the api client is "the only React module allowed to call `invoke` or subscribe to
// Tauri events", so the subscriptions have to be part of the same interface or a component would
// have to reach past it to hear anything.
//
// Two implementations, one interface: `client.ts` over Tauri, `fake.ts` in memory. Every component
// takes this type and neither knows which one it has.

import type {
  AccountStatusResult,
  CodexHooksReport,
  CodexHooksStatus,
  ConsoleDataEvent,
  ConsoleExitEvent,
  ConsoleScrollback,
  ConsoleSummary,
  HostInfo,
  MetricState,
  OpenCodeHooksReport,
  OpenCodeHooksStatus,
  ProjectSummaryResult,
  ProviderId,
  Scope,
  SessionKey,
  SessionListResult,
  SessionsMetricsEvent,
  Settings,
  StatusSnapshot,
  StopResult,
} from "../bindings";

/** Synchronous, idempotent, and safe to call before the async registration has landed. See
 *  `platform/hostEvents.ts` for why that matters on this edge. */
export type Unsubscribe = () => void;

export interface SessionStartArgs {
  provider: ProviderId;
  cwd: string;
  cols: number;
  rows: number;
}

export interface ConsoleOpenArgs {
  sessionKey: SessionKey;
  cwd: string;
  cols: number;
  rows: number;
}

export interface SessionsChangedPayload {
  scope: Scope;
  generatedAtMs: number;
}

export interface PigeonApi {
  hostInfo(): Promise<HostInfo>;
  settingsGet(): Promise<Settings>;
  settingsSet(patch: Partial<Settings>): Promise<Settings>;
  sessionsList(args: { scope: Scope; force?: boolean }): Promise<SessionListResult>;
  projectsSummary(args: { scope: Scope }): Promise<ProjectSummaryResult>;
  sessionMetrics(args: { key: SessionKey }): Promise<MetricState>;
  statusSnapshot(): Promise<StatusSnapshot>;
  accountStatus(args?: { force?: boolean; provider?: ProviderId }): Promise<AccountStatusResult>;
  projectPick(): Promise<{ cwd: string } | null>;
  folderOpen(args: { cwd: string }): Promise<void>;
  sessionStart(args: SessionStartArgs): Promise<{ id: string }>;
  sessionStop(args: { key: SessionKey }): Promise<StopResult>;
  consoleOpen(args: ConsoleOpenArgs): Promise<{ id: string }>;
  consoleReady(args: { id: string }): Promise<void>;
  consoleInput(args: { id: string; dataB64: string }): Promise<void>;
  consoleResize(args: { id: string; cols: number; rows: number }): Promise<void>;
  consoleClose(args: { id: string }): Promise<void>;
  consoleList(): Promise<{ consoles: ConsoleSummary[] }>;
  consoleScrollback(args: { id: string; maxBytes?: number }): Promise<ConsoleScrollback>;
  hoverToggle(): Promise<{ visible: boolean }>;
  hoverSelect(key: SessionKey): Promise<void>;

  /** Whether Pigeon can see Codex waiting on the owner at all. */
  codexHooksStatus(): Promise<CodexHooksStatus>;
  /** Install and trust the Codex hook. One owner decision, one command. */
  codexHooksEnable(): Promise<CodexHooksReport>;
  codexHooksDisable(): Promise<void>;

  /** Whether Pigeon can see OpenCode waiting on the owner at all. */
  opencodeHooksStatus(): Promise<OpenCodeHooksStatus>;
  /** Install the OpenCode permission bridge. One owner decision, one command. */
  opencodeHooksEnable(): Promise<OpenCodeHooksReport>;
  opencodeHooksDisable(): Promise<void>;

  onSessionsChanged(cb: (e: SessionsChangedPayload) => void): Unsubscribe;
  onSessionsMetrics(cb: (e: SessionsMetricsEvent) => void): Unsubscribe;
  onStatusChanged(cb: (e: StatusSnapshot) => void): Unsubscribe;
  onConsoleData(cb: (e: ConsoleDataEvent) => void): Unsubscribe;
  onConsoleExit(cb: (e: ConsoleExitEvent) => void): Unsubscribe;
  onSelectSession(cb: (key: SessionKey) => void): Unsubscribe;
}

/** The slice of the api a terminal needs. `TerminalPanel`/`ConsoleView` take only this, so nothing
 *  in the console path can reach a session or project command by accident. */
export type ConsoleApi = Pick<
  PigeonApi,
  | "consoleReady"
  | "consoleInput"
  | "consoleResize"
  | "consoleClose"
  | "consoleList"
  | "consoleScrollback"
  | "onConsoleData"
  | "onConsoleExit"
>;
