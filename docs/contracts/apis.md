# Pigeon — Rust ↔ React API Contract

**Version:** 0.4 · **Date:** 2026-09-13 · **Transport:** Tauri IPC · **Status:** implementation contract

This is the contract between Pigeon's Rust host and React view. It defines every command, event,
request, response, shared type, loading state, error state, selection rule, project/session behavior,
terminal lifecycle, and refresh rule the UI needs. Provider-specific files, database columns,
credentials, and parser records do not cross this boundary.

## 1. Boundary and ownership

~~~text
Claude/Codex/OpenCode sources
        ↓ provider adapters + mappers
neutral Rust application types
        ↓ Tauri commands and events
React UI
~~~

Rust owns source access, normalization, metrics, KPIs, live status, project aggregation, caching,
PTY processes, credentials, and error classification. React owns rendering, local formatting,
selection, filters, expand/collapse state, and terminal presentation.

The production app has no HTTP server and no REST API. This is a Tauri IPC contract. Vite may serve
assets during development but is not the application API.

## 2. Contract rules

- Command names are exact snake-case names below; payload fields are camelCase.
- Timestamps are UTC epoch milliseconds; React formats them for the local timezone.
- A session is identified by the complete SessionKey: (providerId, sid). sid is never used alone.
- Workspace scope is `live` or `recent`. `live` means a provider terminal/process is currently open
  anywhere on the host. `recent` means a session that is no longer live and whose `closedAtMs` is
  within the last seven days.
- Rust returns provider-neutral objects only. React never parses provider output.
- A command returns its declared result or ApiError. Provider problems affecting one engine are
  returned inside the relevant report so other engines still render.
- null, pending, unavailable, and numeric zero are distinct.
- Unknown provider shapes produce diagnostics and no guessed number.
- Rust source types are authoritative. TypeScript bindings are generated and never hand-edited.

## 3. Shared types

Rust defines these with serde Serialize/Deserialize and specta Type. tauri-specta generates the
TypeScript bindings.

### 3.1 Identity

~~~ts
type ProviderId = "claude-code" | "codex" | "opencode";

interface SessionKey {
  providerId: ProviderId;
  sid: string;
}

interface HostInfo {
  os: "macos" | "windows" | "linux";
  arch: string;
  version: string;
}
~~~

sid is the complete provider session id. Truncation is presentation-only and never allowed for requests,
React keys, comparisons, cache keys, or selection.

### 3.2 Session

~~~ts
interface Session {
  key: SessionKey;
  cwd: string | null;
  project: string;
  projectName: string;
  projectLeaf: string;
  title: string;
  name: string | null;
  gitBranch: string | null;
  firstActiveMs: number | null;
  lastActiveMs: number;
  closedAtMs: number | null;
  resumable: boolean;
  resumeBlockedReason: string | null;
  metrics: MetricState;
  sourceSummary: string | null;
  diagnostics: { unknownTypes: Record<string, number> };
}

interface SessionRow extends Session {
  status: SessionStatus | null;
}

type SessionStatus = "running" | "needs_you" | "finished" | "unknown";

type MetricState =
  | { state: "pending" }
  | { state: "ready"; value: Metrics }
  | { state: "unavailable"; error: EngineError };
~~~

The UI uses Session.key as the React key. Resuming, closing a console, process exit, refresh, cache
eviction, and app restart do not create a new session while the provider source record remains.
`closedAtMs` describes the most recent transition from live to closed. Closing Pigeon's console does
not set it when the provider terminal remains open elsewhere.

### 3.3 Metrics

~~~ts
interface Metrics {
  inputTokens: number;
  outputTokens: number;
  cacheRead: number;
  cacheWrite: number;
  apiCalls: number;
  toolCalls: number;
  userTurns: number;
  durationMs: number | null;
  reasoningTokens: number | null;
  providerCostUsd: number | null;
  kpis: Kpis;
}

interface Kpis {
  contextPerCall: number | null;
  rewriteRatio: number | null;
  batchingRatio: number | null;
}
~~~

The formulas are respectively cacheRead/apiCalls, cacheWrite/cacheRead, and toolCalls/apiCalls.
A zero denominator returns null. A calculation failure is MetricState.unavailable, not zero.
The Rust metrics/API layer owns these calculations. Provider adapters return raw counters only, and
the React View never calculates or stores KPI values. Project KPIs are calculated by the same API
layer after summing the current session counters; session KPIs are never averaged.

### 3.4 Project summaries

~~~ts
interface ProjectSummary {
  project: string;       // normalized working-directory path; stable project key
  projectName: string;   // display name, normally the final path component
  cwd: string;           // path displayed by the project card and detail pane
  projectLeaf: string;
  sessions: number;
  counted: number;
  providers: Partial<Record<ProviderId, number>>;
  statusCounts: StatusCounts;
  totals: MetricsTotals;
  kpis: Kpis;
  lastActiveMs: number;
  costRows: number;
}

interface StatusCounts {
  running: number;
  needsYou: number;
  finished: number;
  unknown: number;
}

interface MetricsTotals {
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
~~~

Project totals are sums over counted sessions. Project KPIs are recomputed from totals, never averaged
from session KPIs. counted less than sessions is visible while lazy counting continues.

### 3.5 Live status

~~~ts
type LiveState = "running" | "needs_you" | "unknown";

interface LiveSessionState {
  key: SessionKey;
  process: "present" | "absent";
  state: LiveState;
  projectLeaf: string | null;
  title: string | null;
  name: string | null;
  sinceMs: number | null;
  rawWord: string | null;
  evidence: string[];
  pid: number | null;
  consoleId: string | null;
  observedAtMs: number;
}

interface StatusSnapshot {
  generatedAtMs: number;
  counts: {
    running: number;
    needsYou: number;
    unknown: number;
  };
  live: LiveSessionState[];
}
~~~

`live` contains only sessions with an active provider process/terminal, regardless of whether Pigeon
currently owns a console for it. A session with no live process is absent from live, has no live badge,
and contributes to no live count. `finished` is a valid display state for a closed/recent session,
but is never returned in `StatusSnapshot.live` or counted in a live scope.

### 3.6 Account and capacity

~~~ts
interface Identity {
  provider: ProviderId;
  signedIn: boolean;
  label: string | null;
  organization: string | null;
  plan: string | null;
  tier: string | null;
  mode: string | null;
  accountShort: string | null;
  providers: { name: string; kind: string }[] | null;
  readAtMs: number;
  problem: EngineError | null;
}

interface Capacity {
  provider: ProviderId;
  supported: boolean;
  windows: CapacityWindow[];
  plan: string | null;
  stale: boolean;
  sourceAgeS: number | null;
  reachedLimit: string | null;
  readAtMs: number;
  problem: EngineError | null;
}

interface CapacityWindow {
  name: "five_hour" | "weekly";
  windowMinutes: number;
  usedPct: number;
  resetsAtMs: number | null;
}

interface AccountStatus {
  provider: ProviderId;
  identity: Identity;
  capacity: Capacity;
}
~~~

Unsupported capacity is supported false with an empty windows list. The UI never draws a zero bar for
unsupported capacity.

### 3.7 Console, settings, and errors

~~~ts
interface ConsoleSummary {
  id: string;
  sessionKey: SessionKey | null;
  provider: ProviderId;
  cwd: string;
  mode: "resume" | "new";
  state: "starting" | "running" | "exited" | "closed";
  cols: number;
  rows: number;
  scrollbackBytes: number;
  exitCode: number | null;
  startedAtMs: number;
}

interface Settings {
  hover: {
    visible: boolean;
    corner: "tl" | "tr" | "bl" | "br" | null;
    x: number | null;
    y: number | null;
  };
  list: {
    view: "live" | "recent";
  };
  pollIntervalSeconds: number;
  recentWindowDays: number;
  split: number;
}

interface EngineError {
  provider: ProviderId | null;
  kind: ErrorKind;
  detail: ErrorDetail;
  message: string;
}

type ErrorKind =
  | "not_installed" | "root_missing" | "no_credential"
  | "credential_refused" | "transport" | "http_status"
  | "unknown_shape" | "stale" | "busy" | "io"
  | "unsupported" | "path" | "process_ambiguous" | "process_stop_failed";

type ErrorDetail =
  | { type: "none" }
  | { type: "status"; code: number }
  | { type: "exit"; code: number }
  | { type: "path"; path: string }
  | { type: "word"; word: string }
  | { type: "fields"; fields: string[] };

interface ApiError {
  code: "INVALID_ARGUMENT" | "NOT_FOUND" | "NOT_READY" | "CONFLICT" | "HOST_FAILURE";
  message: string;
  detail?: Record<string, string | number | boolean>;
}

interface StopResult {
  key: SessionKey;
  stopped: { pid: number; evidence: string }[];
  alreadyStopped: boolean;
  ambiguous: { pid: number; reason: string }[];
}
~~~

`pollIntervalSeconds` defaults to 5 and is valid from 1 through 300. `recentWindowDays` defaults to
7 and is valid from 1 through 365. Invalid settings revert to defaults. Polling compares current
source signatures/state in memory and does not write unchanged rows.

Credentials, tokens, headers, raw response bodies, and raw exception text never enter any payload.

## 4. Commands

All commands are Tauri invoke calls. Generated bindings should wrap these names.

### host_info

~~~ts
host_info(): Promise<HostInfo>
~~~

Called during startup. The UI uses this as its only platform signal.

### sessions_list

~~~ts
sessions_list(args: { scope: "live" | "recent"; force?: boolean }): Promise<{
  scope: "live" | "recent";
  sinceMs: number | null;
  rows: SessionRow[];
  problems: EngineError[];
  generatedAtMs: number;
}>
~~~

For `live`, returns provider sessions with an active provider process/terminal. For `recent`, returns
sessions that are no longer live and whose `closedAtMs` is within the configured seven-day window.
Results are merged and sorted by the most relevant activity timestamp descending, then provider, then
full sid. It does not perform full metric folds. force bypasses the ten-second sessions cache.

### session_metrics

~~~ts
session_metrics(args: { key: SessionKey }): Promise<MetricState>
~~~

Returns the selected session's current metric state. It may calculate asynchronously in Rust. The
same source signature and counting rules must produce the same result after resume, console close,
cache eviction, or app restart.

### projects_summary

~~~ts
projects_summary(args: { scope: "live" | "recent" }): Promise<{
  scope: "live" | "recent";
  sinceMs: number | null;
  projects: ProjectSummary[];
  generatedAtMs: number;
}>
~~~

Groups by normalized working-directory path. Project names are display context, not session ids.

`live` includes only projects with at least one live session. `recent` includes only projects with
one or more closed sessions whose `closedAtMs` is within `settings.recentWindowDays`. Sessions outside
the selected scope are not returned or counted. For a live project, `sessions` is the number of live
sessions in that project; it is not a historical total. For a recent project, `sessions` is the number
of recently closed sessions.

### project_pick, folder_open, and session_start

~~~ts
project_pick(): Promise<{ cwd: string } | null>
folder_open(args: { cwd: string }): Promise<void>
session_start(args: {
  provider: ProviderId;
  cwd: string;
  cols: number;
  rows: number;
}): Promise<{ id: string }>
~~~

`project_pick` uses the native folder picker. `session_start` launches the selected installed CLI in
that folder with no resume argument. It creates a Pigeon-owned console, not a Pigeon session; the
new `SessionKey` appears when the engine writes a discoverable source record.
`folder_open` opens the supplied project directory using the host's default folder application. It
does not create or select a session.

### session_stop

~~~ts
session_stop(args: { key: SessionKey }): Promise<StopResult>
~~~

The host stops every process it can prove belongs to the session, including processes started outside
Pigeon. It refuses ambiguous matches, never guesses from cwd or executable name alone, and leaves
source files untouched. The operation is idempotent when no matching process remains.

### account_status

~~~ts
account_status(args?: {
  force?: boolean;
  provider?: ProviderId;
}): Promise<{
  accounts: Partial<Record<ProviderId, AccountStatus>>;
  generatedAtMs: number;
}>
~~~

force bypasses account cache. provider limits refresh to one provider.

### status_snapshot

~~~ts
status_snapshot(): Promise<StatusSnapshot>
~~~

Returns the current canonical status projection. Only sessions with an active provider process/terminal
are included in `live`; status-less and closed sessions are omitted.

### Console commands

~~~ts
console_open(args: {
  sessionKey: SessionKey;
  cwd: string;
  cols: number;
  rows: number;
}): Promise<{ id: string }>

console_ready(args: { id: string }): Promise<void>
console_input(args: { id: string; dataB64: string }): Promise<void>
console_resize(args: { id: string; cols: number; rows: number }): Promise<void>
console_close(args: { id: string }): Promise<void>
console_list(): Promise<{ consoles: ConsoleSummary[] }>
console_scrollback(args: { id: string; maxBytes?: number }): Promise<{
  id: string;
  dataB64: string;
  truncated: boolean;
}>
~~~

console_open validates sessionKey and host-resolved cwd, resolves sessionKey.providerId on the effective
PATH, launches its resume command in a PTY, and returns a new console id. It never creates a session.
`console_list` may include consoles detached from the visible pane. Selecting their session calls
`console_scrollback` before subscribing to live output. `console_scrollback` returns bounded UTF-8-safe
terminal bytes encoded as base64; the UI writes them to its terminal renderer before live bytes. Closing
the application terminates every Pigeon-owned console.

The UI attaches in this order:

~~~text
subscribe console data and exit
→ create terminal
→ fit
→ console_resize
→ console_list
→ console_scrollback for an existing detached console
→ console_ready last
~~~

console_close ends the PTY process but does not delete or alter the engine session. Unknown ids
return ApiError code NOT_FOUND. dataB64 is base64 because terminal data is bytes.

### Hover and settings

~~~ts
hover_toggle(): Promise<{ visible: boolean }>
hover_select(args: SessionKey): Promise<void>
settings_get(): Promise<Settings>
settings_set(args: Partial<Settings>): Promise<Settings>
~~~

hover_select brings the main window forward and emits feather://select-session.

## 5. Events

~~~ts
"sessions://changed": {
  scope: "live" | "recent";
  generatedAtMs: number;
}

"sessions://metrics": {
  rows: { key: SessionKey; metrics: MetricState }[];
  generatedAtMs: number;
}

"status://changed": StatusSnapshot

"capacity://changed": {
  provider: ProviderId;
  account: AccountStatus;
  generatedAtMs: number;
}

"console://data": {
  id: string;
  dataB64: string;
}

"console://exit": {
  id: string;
  exitCode: number | null;
}

"feather://select-session": SessionKey
~~~

Metric events arrive in batches of at most 20 rows. React merges them by SessionKey. Status events
replace the status snapshot atomically. Events are emitted only when their underlying value changes,
except console data and exit.

## 6. UI interaction contracts

### Startup

~~~text
host_info + settings_get + sessions_list({scope:"live"}) + projects_summary({scope:"live"})
  + status_snapshot in parallel
→ render Live project cards with expanded live sessions
→ start status refresh
→ accept sessions://metrics as lazy metrics arrive
~~~

The list renders before metric folds complete. Pending metrics show counting, never blank or zero.
Selecting Recent repeats the list and project queries with `scope: "recent"`; the host applies the
closed-at seven-day cutoff and omits projects with no qualifying session. Account/capacity data is
not part of the current dashboard mockup and is not required for dashboard startup.

### Project selection

Clicking a project selects its normalized project key and renders ProjectSummary totals and KPIs
in the right pane. The session list remains visible. The project path is shown from
`ProjectSummary.cwd`.

### Session selection

Clicking a session selects its SessionKey and renders that session's MetricState, live status,
evidence, source summary, and Resume action. The project remains visible as context.

### Resume in the same pane

Clicking Resume calls `console_open` for the selected SessionKey. Add session calls `session_start`
with the selected project cwd and chosen engine. Open folder calls `folder_open` with the project cwd.
The right pane keeps the session title, project, status, and metrics above the embedded terminal. It
does not open a new page or browser tab.

### Session stop and terminal continuity

Clicking Stop session calls `session_stop`. A successful result refreshes the scoped project/session
view and leaves source data unchanged. Selecting another session detaches the visible terminal only;
it does not call `console_close`. Returning selects the existing console from `console_list`, replays
its bounded scrollback, and resumes live output. `console_close` is used only when the owner closes
the terminal or when Pigeon shuts down.

### Hover selection

hover_select(SessionKey) focuses main and emits feather://select-session. React selects and scrolls
to the exact row.

## 7. Rust module boundary

~~~text
src-tauri/src/
  api/
    types.rs       neutral structs/enums
    commands.rs    Tauri commands
    events.rs      event payloads
    errors.rs      API/domain errors
    bindings.rs    tauri-specta export
  domain/          session, metrics, status, project types
  adapters/        claude, codex, opencode parsing/mapping
  services/        sessions, metrics, accounts, status, console
  app_state.rs     shared in-memory state

src/bindings.ts     generated; never hand-edit
~~~

Commands are thin orchestration functions. They validate inputs, call services, and return declared
types. Provider parsing and business rules do not belong in command functions.

## 8. Verification requirements

- Rust binding generation succeeds and produces no uncommitted diff.
- TypeScript compiles using generated bindings.
- Every command has success and error fixtures.
- Every event has a serialization fixture.
- SessionKey survives refresh, resume, console close, process exit, cache eviction, and restart.
- Identical source signatures produce identical metrics and KPIs.
- Project totals are sums and project KPIs are recomputed from totals.
- Status counts equal live entries; status-less sessions are excluded.
- Console data is base64 and exit is emitted once.
- Detached console scrollback is bounded, replayed before live output, and never persisted in the
  relational tables.
- Credentials are absent from responses, events, errors, and logs.

## 9. Source requirements

This contract implements the view-facing portions of
[urds.md](urds.md) and
[frds.md](frds.md), especially UR-1 through UR-9,
P1–P8, FR-1, FR-5 through FR-10, FR-11 through FR-26, and FR-30 through FR-33.
