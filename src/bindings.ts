// The Rust ↔ React wire contract, mirroring `docs/contracts/apis.md` and the serde attributes in
// `src-tauri/src/api/types.rs`.
//
// Rust is authoritative. When a shape changes there, it changes here in the same commit, and
// `src-tauri/src/api/types.rs`'s serialization tests are what prove the two agree.

export type ProviderId = "claude-code" | "codex" | "opencode";

export const PROVIDER_IDS: ProviderId[] = ["claude-code", "codex", "opencode"];

/** Display names. Never parsed, never used as an identity. */
export const PROVIDER_LABELS: Record<ProviderId, string> = {
  "claude-code": "Claude Code",
  codex: "Codex",
  opencode: "OpenCode",
};

/**
 * A session's complete identity. `sid` is NEVER used alone — not as a React key, a map key, or a
 * command argument. Two engines may legitimately mint the same uuid.
 */
export interface SessionKey {
  providerId: ProviderId;
  sid: string;
}

/** The one key function. Every map, every React key, every comparison goes through it. */
export function sessionKeyId(key: SessionKey): string {
  return `${key.providerId}:${key.sid}`;
}

export function sameSession(a: SessionKey | null, b: SessionKey | null): boolean {
  if (!a || !b) return false;
  return a.providerId === b.providerId && a.sid === b.sid;
}

export interface HostInfo {
  os: string;
  arch: string;
  version: string;
}

/** `live` = an engine process is open now. `recent` = closed inside the configured window. */
export type Scope = "live" | "recent";

export type SessionStatus = "running" | "delegating" | "needs_you" | "finished" | "unknown";
export type LiveState = "running" | "delegating" | "waiting" | "needs_you" | "unknown";

export interface Kpis {
  /** cacheRead ÷ apiCalls — how heavy each turn was. */
  contextPerCall: number | null;
  /** cacheWrite ÷ cacheRead — stale-resume churn. Low is good. */
  rewriteRatio: number | null;
  /** toolCalls ÷ apiCalls — parallel-call discipline. */
  batchingRatio: number | null;
}

export interface Metrics {
  inputTokens: number;
  outputTokens: number;
  cacheRead: number;
  cacheWrite: number;
  apiCalls: number;
  toolCalls: number;
  userTurns: number;
  durationMs: number | null;
  reasoningTokens: number | null;
  /** The engine's OWN figure where it states one. Pigeon never invents a price. */
  providerCostUsd: number | null;
  kpis: Kpis;
}

/** Pending, ready and unavailable are three different things, and none of them is zero. */
export type MetricState =
  | { state: "pending" }
  | { state: "ready"; value: Metrics }
  | { state: "unavailable"; error: EngineError };

export interface Diagnostics {
  unknownTypes: Record<string, number>;
}

export interface Session {
  key: SessionKey;
  cwd: string | null;
  project: string;
  projectName: string;
  repositoryName?: string;
  projectLeaf: string;
  title: string;
  name: string | null;
  gitBranch: string | null;
  firstActiveMs: number | null;
  lastActiveMs: number;
  closedAtMs: number | null;
  resumable: boolean;
  /** Already a sentence when present. The View renders it, never parses it. */
  resumeBlockedReason: string | null;
  metrics: MetricState;
  sourceSummary: string | null;
  diagnostics: Diagnostics;
}

export interface SessionRow extends Session {
  /** Joined by the complete SessionKey, never by sid alone. */
  status: SessionStatus | null;
}

export interface StatusCounts {
  running: number;
  needsYou: number;
  finished: number;
  unknown: number;
}

export interface MetricsTotals {
  inputTokens: number;
  outputTokens: number;
  cacheRead: number;
  cacheWrite: number;
  apiCalls: number;
  toolCalls: number;
  userTurns: number;
  durationMs: number;
  reasoningTokens: number;
  providerCostUsd: number;
}

export interface ProjectSummary {
  project: string;
  projectName: string;
  cwd: string;
  projectLeaf: string;
  sessions: number;
  /** How many of `sessions` have landed metrics. Below `sessions` renders as partial. */
  counted: number;
  providers: Partial<Record<ProviderId, number>>;
  statusCounts: StatusCounts;
  totals: MetricsTotals;
  /** Recomputed from `totals`, never averaged from the rows' own KPIs. */
  kpis: Kpis;
  lastActiveMs: number;
  /** How many rows contributed a provider-stated cost. Zero hides the cost figure. */
  costRows: number;
}

export interface LiveSessionState {
  key: SessionKey;
  process: "present" | "absent";
  state: LiveState;
  projectLeaf: string | null;
  title: string | null;
  name: string | null;
  sinceMs: number | null;
  /** The engine's own word, quoted back rather than translated away. */
  rawWord: string | null;
  evidence: string[];
  pid: number | null;
  consoleId: string | null;
  activeSubagents: number;
  observedAtMs: number;
}

export interface StatusSnapshot {
  generatedAtMs: number;
  counts: { running: number; needsYou: number; unknown: number };
  /** Only sessions with a live engine process. A closed session is never in here. */
  live: LiveSessionState[];
}

export interface ProviderSummaryEntry {
  name: string;
  kind: string;
}

export interface Identity {
  provider: ProviderId;
  signedIn: boolean;
  label: string | null;
  organization: string | null;
  plan: string | null;
  tier: string | null;
  mode: string | null;
  accountShort: string | null;
  providers: ProviderSummaryEntry[] | null;
  readAtMs: number;
  problem: EngineError | null;
}

export interface CapacityWindow {
  name: "five_hour" | "weekly" | "monthly";
  windowMinutes: number;
  usedPct: number;
  resetsAtMs: number | null;
}

export interface Capacity {
  provider: ProviderId;
  /** False means the engine publishes no allowance. Render "n/a", never a zero bar. */
  supported: boolean;
  windows: CapacityWindow[];
  plan: string | null;
  stale: boolean;
  sourceAgeS: number | null;
  reachedLimit: string | null;
  readAtMs: number;
  problem: EngineError | null;
}

export interface AccountStatus {
  provider: ProviderId;
  identity: Identity;
  capacity: Capacity;
}

export type ConsoleMode = "resume" | "new";

/**
 * `"closed"` is declared by the contract but never observed: the host removes a closed console
 * from its registry rather than marking it. The states that actually arrive are `starting`
 * (spawned, output still gated), `running`, and `exited`. Treat a console disappearing from
 * `console_list` as the closed signal.
 */
export type ConsoleState = "starting" | "running" | "exited" | "closed";

export interface ConsoleSummary {
  id: string;
  sessionKey: SessionKey | null;
  provider: ProviderId;
  cwd: string;
  mode: ConsoleMode;
  state: ConsoleState;
  cols: number;
  rows: number;
  scrollbackBytes: number;
  exitCode: number | null;
  startedAtMs: number;
}

export interface ConsoleScrollback {
  id: string;
  dataB64: string;
  truncated: boolean;
}

export type ErrorKind =
  | "not_installed"
  | "root_missing"
  | "no_credential"
  | "credential_refused"
  | "transport"
  | "http_status"
  | "unknown_shape"
  | "stale"
  | "busy"
  | "io"
  | "unsupported"
  | "path"
  | "process_ambiguous"
  | "process_stop_failed";

export type ErrorDetail =
  | { type: "none" }
  | { type: "status"; code: number }
  | { type: "exit"; code: number }
  | { type: "path"; path: string }
  | { type: "word"; word: string }
  | { type: "fields"; fields: string[] };

export interface EngineError {
  provider: ProviderId | null;
  kind: ErrorKind;
  detail: ErrorDetail;
  /** Built in Rust from the kind and the typed detail. Never a source error's own text. */
  message: string;
}

export interface ApiError {
  code: "INVALID_ARGUMENT" | "NOT_FOUND" | "NOT_READY" | "CONFLICT" | "HOST_FAILURE";
  message: string;
  detail?: Record<string, string | number | boolean>;
}

export interface StopResult {
  key: SessionKey;
  stopped: { pid: number; evidence: string }[];
  alreadyStopped: boolean;
  ambiguous: { pid: number; reason: string }[];
}

/** Whether Pigeon can see Codex waiting on the owner. */
export interface CodexHooksStatus {
  installed: boolean;
}

/**
 * What one enable attempt did. `trusted` is the field that matters: Codex silently skips an
 * untrusted hook, so `installed && !trusted` means the feature is present and doing nothing.
 */
export interface CodexHooksReport {
  installed: boolean;
  trusted: boolean;
  message: string;
}

export interface Settings {
  hover: {
    visible: boolean;
    corner: "tl" | "tr" | "bl" | "br" | null;
    x: number | null;
    y: number | null;
  };
  list: { view: Scope };
  pollIntervalSeconds: number;
  recentWindowDays: number;
  split: number;
}

export interface SessionListResult {
  scope: Scope;
  sinceMs: number | null;
  rows: SessionRow[];
  /** One engine's failure travels here so the other two still render. */
  problems: EngineError[];
  generatedAtMs: number;
}

export interface ProjectSummaryResult {
  scope: Scope;
  sinceMs: number | null;
  projects: ProjectSummary[];
  generatedAtMs: number;
}

export interface AccountStatusResult {
  accounts: Partial<Record<ProviderId, AccountStatus>>;
  generatedAtMs: number;
}

// ---- event topics, frozen seam -----------------------------------------------------------

export const EV_SESSIONS_CHANGED = "sessions://changed";
export const EV_SESSIONS_METRICS = "sessions://metrics";
export const EV_STATUS_CHANGED = "status://changed";
export const EV_CAPACITY_CHANGED = "capacity://changed";
export const EV_CONSOLE_DATA = "console://data";
export const EV_CONSOLE_EXIT = "console://exit";
export const EV_SELECT_SESSION = "feather://select-session";

export interface SessionsChangedEvent {
  scope: Scope;
  generatedAtMs: number;
}

export interface SessionsMetricsEvent {
  rows: { key: SessionKey; metrics: MetricState }[];
  generatedAtMs: number;
}

export interface CapacityChangedEvent {
  provider: ProviderId;
  account: AccountStatus;
  generatedAtMs: number;
}

export interface ConsoleDataEvent {
  id: string;
  dataB64: string;
}

export interface ConsoleExitEvent {
  id: string;
  exitCode: number | null;
}
