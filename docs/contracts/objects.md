# Pigeon — Object Model Contract

**Version:** 0.2 · **Date:** 2026-09-13 · **Status:** implementation contract

This document defines the Rust objects used between provider adapters, the current-state relational
model, application services, and the Tauri API. It is intentionally separate from
[`data.md`](data.md): the relational model describes storage, while this document
describes behavior, ownership, and transformations.

The object model must remain small. Provider-specific parser objects are private implementation
details. React receives API DTOs only. Database rows are never passed directly to React.

## 1. Object flow

```text
provider source
    ↓ parse
private provider objects
    ↓ map
canonical Rust domain objects
    ↓ persist/project
relational rows and API DTOs
    ↓ serialize
Tauri events/command responses
    ↓
React
```

There are four object boundaries:

1. **Provider objects** — private structs matching Claude, Codex, or OpenCode input shapes.
2. **Domain objects** — provider-neutral Rust structs and enums used by services.
3. **Persistence objects** — typed representations of the three current-state tables.
4. **API objects** — Tauri DTOs shaped for the React contract.

The first implementation may keep persistence objects in memory. The boundary still exists so an
SQLite implementation can be added without changing provider adapters or React.

## 2. Design rules

- `ProviderId` is the only provider discriminator visible outside an adapter.
- A session is identified only by `SessionKey { provider_id, sid }`.
- `sid` must never be used alone as a map key, React key, cache key, or command argument.
- Provider raw records never cross the adapter boundary.
- Provider adapters produce neutral drafts; services apply shared normalization and business rules.
- Metrics are counters plus derived KPIs. KPIs are never independently stored.
- Live state is an observation, not a permanent fact about a session.
- A console is a runtime PTY handle, not a session and not a durable history record.
- Terminal bytes and scrollback belong to the runtime console object; terminal rendering belongs to
  React. Rust owns the PTY, bounded scrollback, lifecycle, and byte transport.
- `None`/`null`, pending, unavailable, and numeric zero have different meanings.
- All timestamps inside Rust are UTC epoch milliseconds.
- Objects crossing Tauri use `serde` serialization and camelCase wire names.

## 3. Shared value objects

These objects have no independent lifecycle or database row.

### 3.1 ProviderId

```rust
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderId {
    ClaudeCode,
    Codex,
    OpenCode,
}
```

Wire values are `claude-code`, `codex`, and `opencode`.

`ProviderId` selects the adapter and namespaces session IDs. It does not cause provider-specific
columns to appear in the relational model.

### 3.2 SessionKey

```rust
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, Type)]
pub struct SessionKey {
    pub provider_id: ProviderId,
    pub sid: String,
}
```

Rules:

- `sid` must be the complete provider session ID.
- Empty or whitespace-only `sid` is invalid.
- Display truncation is allowed only in UI formatting.
- The key remains unchanged when a provider creates another transcript file for the same logical
  session, as Codex does on resume.

### 3.3 ProjectKey

```rust
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProjectKey(pub String);
```

The value is the normalized absolute working-directory path. The reserved key
`__no_directory__` represents a discovered session with no usable working directory.

Normalization is:

1. make the path absolute when possible;
2. remove trailing separators;
3. collapse `.` and `..` segments;
4. case-fold on Windows only;
5. use `__no_directory__` when no directory exists in the source record.

### 3.4 SourceSignature

```rust
pub enum SourceSignature {
    Claude { main: FileSignature, sidecar_mtime_ms: Option<i64> },
    Codex { files: Vec<FileSignature> },
    OpenCode { time_updated_ms: i64 },
}

pub struct FileSignature {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ms: i64,
}
```

This is runtime cache identity. It is not persisted and never sent to React.

## 4. Canonical domain objects

### 4.1 Session

```rust
pub struct Session {
    pub key: SessionKey,
    pub cwd: Option<PathBuf>,
    pub project: ProjectKey,
    pub title: String,
    pub name: Option<String>,
    pub git_branch: Option<String>,
    pub first_active_ms: Option<i64>,
    pub last_active_ms: i64,
    pub closed_at_ms: Option<i64>,
    pub resumable: bool,
    pub resume_blocked_reason: Option<ResumeBlockedReason>,
    pub metrics: MetricState,
    pub source: SourceSummary,
    pub diagnostics: Diagnostics,
}
```

`Session` is provider-neutral current session metadata. It does not contain process status. Status
is joined from `LiveObservation` by `SessionKey`. `closed_at_ms` is the most recent transition from
live to closed; closing a Pigeon console does not set it if the provider process remains open
elsewhere.

### 4.2 SourceSummary and Diagnostics

```rust
pub struct SourceSummary {
    pub paths: Vec<PathBuf>,
    pub files: u32,
}

pub struct Diagnostics {
    pub unknown_types: BTreeMap<String, u32>,
}
```

These are bounded projections. Raw provider records, raw JSON, and transcript text do not belong
here.

### 4.3 ResumeBlockedReason

```rust
pub enum ResumeBlockedReason {
    MissingWorkingDirectory,
    WorkingDirectoryMissing,
    ProviderNotInstalled,
    InvalidSessionId,
}
```

The API converts these to safe display strings. The object must not contain command output or raw
provider errors.

### 4.4 Metrics and KPIs

```rust
pub struct Metrics {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub api_calls: u64,
    pub tool_calls: u64,
    pub user_turns: u64,
    pub duration_ms: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub engine_cost_usd: Option<f64>,
}

pub struct Kpis {
    pub context_per_call: Option<f64>,
    pub rewrite_ratio: Option<f64>,
    pub batching_ratio: Option<f64>,
}

pub enum MetricState {
    Pending,
    Ready { metrics: Metrics, basis: MetricBasis, counted_at_ms: i64 },
    Unavailable { error: EngineError },
}

pub enum MetricBasis {
    Fold,
    Deltas,
    Columns,
}
```

The formulas are owned by the metrics service:

```text
context_per_call = cache_read / api_calls
rewrite_ratio    = cache_write / cache_read
batching_ratio   = tool_calls / api_calls
```

A zero denominator produces `None`. A failed calculation or provider drift produces
`MetricState::Unavailable`, never fabricated zeroes.

Project KPIs are calculated after summing project counters. Session KPIs are never averaged to
produce project KPIs.

### 4.5 LiveObservation

```rust
pub struct LiveObservation {
    pub key: SessionKey,
    pub process: ProcessPresence,
    pub state: LiveState,
    pub since_ms: Option<i64>,
    pub raw_word: Option<String>,
    pub evidence: Vec<String>,
    pub pid: Option<u32>,
    pub console_id: Option<String>,
    pub observed_at_ms: i64,
}

pub enum ProcessPresence { Present, Absent }

pub enum LiveState { Running, NeedsYou, Unknown }
```

Rules:

- At most one current observation exists per `SessionKey`.
- `process = Absent` means no status badge and no status count.
- A provider status-source failure becomes an `EngineError`; it is not converted into a fake
  `Unknown` session state.
- A hosted console can establish `Running` when its process is proven, even if a provider status
  file is stale or missing.
- `LiveObservation` exists only while a provider process/terminal is open somewhere on the host.
  Closed sessions are represented by `Session.closed_at_ms` and may appear in Recent, not as live
  observations.

### 4.6 AccountStatus

```rust
pub struct AccountStatus {
    pub provider: ProviderId,
    pub identity: Identity,
    pub capacity: Capacity,
}

pub struct Identity {
    pub provider: ProviderId,
    pub signed_in: bool,
    pub label: Option<String>,
    pub organization: Option<String>,
    pub plan: Option<String>,
    pub tier: Option<String>,
    pub mode: Option<String>,
    pub account_short: Option<String>,
    pub providers: Option<Vec<ProviderSummary>>,
    pub read_at_ms: i64,
    pub problem: Option<EngineError>,
}

pub struct ProviderSummary { pub name: String, pub kind: String }

pub struct Capacity {
    pub provider: ProviderId,
    pub supported: bool,
    pub windows: Vec<CapacityWindow>,
    pub plan: Option<String>,
    pub stale: bool,
    pub source_age_s: Option<u64>,
    pub reached_limit: Option<String>,
    pub read_at_ms: i64,
    pub problem: Option<EngineError>,
}

pub struct CapacityWindow {
    pub name: CapacityWindowName,
    pub window_minutes: u32,
    pub used_pct: f64,
    pub resets_at_ms: Option<i64>,
}

pub enum CapacityWindowName { FiveHour, Weekly }
```

Credentials and tokens are never fields on these objects. A provider may return
`supported = false` with no windows.

### 4.7 ProjectSummary

```rust
pub struct ProjectSummary {
    pub project: ProjectKey,
    pub project_name: String,
    pub cwd: PathBuf,
    pub project_leaf: String,
    pub sessions: u32,
    pub counted: u32,
    pub providers: BTreeMap<ProviderId, u32>,
    pub status_counts: StatusCounts,
    pub totals: MetricsTotals,
    pub kpis: Kpis,
    pub last_active_ms: i64,
    pub cost_rows: u32,
}

pub struct StatusCounts {
    pub running: u32,
    pub needs_you: u32,
    pub finished: u32,
    pub unknown: u32,
}

pub struct MetricsTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub api_calls: u64,
    pub tool_calls: u64,
    pub user_turns: u64,
    pub duration_ms: u64,
    pub reasoning_tokens: u64,
    pub engine_cost_usd: f64,
}
```

`ProjectSummary` is derived from sessions in the requested Live or Recent scope. It is never an
independent source of truth. In Live scope, `sessions` counts only currently live sessions. In Recent
scope, it counts only closed sessions whose `closed_at_ms` is within the configured window.

### 4.8 Console

```rust
pub struct Console {
    pub id: String,
    pub session_key: Option<SessionKey>,
    pub provider: ProviderId,
    pub cwd: PathBuf,
    pub mode: ConsoleMode,
    pub state: ConsoleState,
    pub size: TerminalSize,
    pub scrollback: ScrollbackBuffer,
    pub exit_code: Option<i32>,
    pub started_at_ms: i64,
}

pub enum ConsoleMode { Resume, New }

pub enum ConsoleState { Starting, Running, Exited, Closed }

pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

pub struct ScrollbackBuffer {
    pub bytes: Vec<u8>,
    pub max_bytes: usize,
    pub truncated: bool,
}
```

`Console` is the Rust runtime object behind the embedded terminal. It owns the PTY process handle,
its lifecycle state, current terminal size, and a bounded byte scrollback buffer. It does not own the
terminal screen model, cursor rendering, selection, colors, or React component state.

The terminal display boundary is:

```text
Rust Console/PTY
  → console://data { id, dataB64 }
  → React terminal adapter (xterm or equivalent)
  → terminal DOM/canvas

Rust ScrollbackBuffer
  → console_scrollback(id)
  → React writes replay bytes before live bytes
```

React owns the visible terminal instance, active console selection, fit calculation, local scroll
position, and input event wiring. Rust owns process input/output, resize forwarding, bounded replay,
console attachment, and shutdown. A console may be detached from the visible pane while remaining
running.

A resumed console has a `SessionKey`. A new console begins with `session_key = None` and is linked
when provider discovery finds the new session. The console registry and scrollback remain runtime
objects and are not database tables.

## 5. Provider adapter objects

Provider adapters may use any private parser structs required by their source format. They expose
only neutral results to the application services.

```rust
pub trait ProviderAdapter: Send + Sync {
    fn provider(&self) -> ProviderId;
    fn discover_sessions(&self) -> Result<ProviderSessionReport, EngineError>;
    fn read_identity(&self) -> Result<Identity, EngineError>;
    fn read_capacity(&self) -> Result<Capacity, EngineError>;
    fn resume_command(&self, sid: &str) -> Result<ResumeCommand, EngineError>;
}
```

Optional capabilities are represented by results, not separate provider-shaped interfaces exposed
to the rest of the app:

```rust
pub struct ProviderSessionReport {
    pub sessions: Vec<SessionCandidate>,
    pub problem: Option<EngineError>,
}

pub struct SessionCandidate {
    pub key: SessionKey,
    pub cwd: Option<PathBuf>,
    pub title: String,
    pub name: Option<String>,
    pub git_branch: Option<String>,
    pub first_active_ms: Option<i64>,
    pub last_active_ms: i64,
    pub closed_at_ms: Option<i64>,
    pub resumable: bool,
    pub resume_blocked_reason: Option<ResumeBlockedReason>,
    pub source: SourceSummary,
    pub diagnostics: Diagnostics,
    pub source_signature: SourceSignature,
}

pub struct ResumeCommand {
    pub program: String,
    pub args: Vec<OsString>,
}
```

The adapter owns provider parsing. The session service owns project normalization, account
association, sorting, scope filtering, and conversion from `SessionCandidate` to `Session`.

Provider mapping summary:

| Provider | Session source | Metrics source | Live source | Account/capacity source |
|---|---|---|---|---|
| Claude Code | JSONL heads | JSONL fold, including sidecars | `~/.claude/sessions/*.json` + process check | `~/.claude.json`, credentials/Keychain, usage endpoint |
| Codex | rollout heads grouped by thread ID | full grouped rollout deltas | process argv/lsof + rollout tail | `~/.codex/auth.json`, rollout rate limits |
| OpenCode | read-only SQLite `session` rows | session columns and `part` rows | process cwd + SQLite state/permission | `auth.json` + safe account email |

## 6. Persistence objects

Persistence objects are storage adapters, not domain truth. They use database names and nullable
columns, then convert into domain objects.

```rust
pub struct AccountRow { /* accounts columns */ }
pub struct ProjectRow { /* projects columns */ }
pub struct SessionRow { /* sessions columns */ }
```

Required conversions:

```rust
impl AccountRow {
    fn into_domain(self) -> Result<AccountStatus, ModelError>;
    fn from_domain(value: &AccountStatus) -> Self;
}

impl ProjectRow {
    fn into_key(&self) -> Result<ProjectKey, ModelError>;
}

impl SessionRow {
    fn into_domain(self, project: ProjectRow) -> Result<Session, ModelError>;
    fn from_session(value: &Session, live: Option<&LiveObservation>) -> Self;
}
```

Persistence rules:

- `(provider_id, sid)` maps exactly to `SessionKey`.
- `metrics_state = pending` has no metric counters.
- `metrics_state = ready` has valid current counters and no metric error.
- `metrics_state = unavailable` has an error kind/detail and must not be rendered as zero metrics.
- `process_present = 0` has no live badge; its live fields are cleared or ignored and
  `closed_at_ms` records the live-to-closed transition when known.
- Every session has a valid `project_id`; missing `cwd` uses `__no_directory__`.
- `account_id` is assigned by matching the session provider to the unique current account when
  known; otherwise it is null.
- KPIs are not columns and are calculated during projection.
- source signatures, console registry state, and scrollback are not persisted.

## 7. API DTO objects

API DTOs are the only objects serialized through Tauri. Their field names and nullability must match
[`apis.md`](apis.md).

```rust
pub struct SessionDto { /* API Session */ }
pub struct SessionRowDto { /* SessionDto plus status */ }
pub struct ProjectSummaryDto { /* ProjectSummary */ }
pub struct StatusSnapshotDto { /* counts plus live rows */ }
pub struct AccountStatusDto { /* account identity and capacity */ }
pub struct ConsoleSummaryDto { /* Console without PTY internals */ }

pub struct ConsoleScrollbackDto {
    pub id: String,
    pub data_b64: String,
    pub truncated: bool,
}
```

DTO conversion rules:

- Rust snake_case fields serialize to the contract's camelCase fields.
- `SessionRowDto.status` is joined by `SessionKey`; the persistence row is not exposed directly.
- `MetricState::Pending` becomes `{ state: "pending" }`.
- `MetricState::Ready` becomes `{ state: "ready", value: Metrics }`.
- `MetricState::Unavailable` becomes `{ state: "unavailable", error: EngineError }`.
- `PathBuf` becomes a normalized string path.
- `Console` becomes a `ConsoleSummaryDto` without PTY handles or raw scrollback bytes. Bounded
  scrollback is returned only through the explicit `console_scrollback` response.
- `BTreeMap<ProviderId, u32>` becomes the API `providers` object.
- typed internal errors become safe `EngineError` or `ApiError`; raw exception text is dropped.
- secrets, tokens, headers, transcript text, source signatures, and PTY bytes never enter ordinary
  DTOs.

## 8. Services and ownership

```text
ProviderAdapter
  → DiscoveryService       → SessionCandidate
  → MetricsService         → MetricState / Metrics / Kpis
  → StatusService          → LiveObservation / StatusSnapshot
  → AccountService         → AccountStatus
  → ConsoleService         → Console
  → ProjectService         → ProjectSummary
  → ProjectionService      → API DTOs
```

Ownership rules:

- `DiscoveryService` creates or updates session metadata.
- `MetricsService` owns folds, deltas, signatures, cache, and KPI formulas.
- `StatusService` owns observations, process attribution, and status counts.
- `AccountService` owns identity/capacity reads and account cache.
- `ProjectService` owns normalization, scope filtering, and rollups.
- `ConsoleService` owns PTYs, console IDs, scrollback, attach, detach, and termination.
- `ProjectionService` assembles DTOs and never reads provider files directly.
- Tauri command functions validate arguments and call services; they contain no provider parsing or
  business rules.

## 9. Lifecycle invariants

### Existing session resume

```text
SessionKey exists
    → console_open(SessionKey, cwd)
    → Console { mode: Resume, session_key: Some(SessionKey) }
    → provider process runs
    → metrics remain associated with SessionKey
    → console close or process exit
    → same SessionKey remains in sessions
```

Resume never creates a second session object and never resets counters or KPIs.

### New session

```text
session_start(provider, cwd)
    → Console { mode: New, session_key: None }
    → provider writes source record
    → discovery finds complete sid
    → Console.session_key is linked to SessionKey
    → sessions row is inserted
    → metrics start Pending, then Ready or Unavailable
```

### Reopen after close

Closing a console removes only runtime PTY state. Reopening the same provider session creates a new
console ID but reuses the same `SessionKey`, source signature, metrics rules, and project.

### Refresh and restart

Refresh reconstructs domain objects from provider sources. Restart reconstructs them again. Neither
operation changes `SessionKey`; identical source signatures and counting rules produce identical
metrics.

## 10. Required tests

- Provider fixtures map into neutral `SessionCandidate` values with no provider fields leaking out.
- Two providers using the same `sid` produce two distinct `SessionKey` values.
- Resume creates a new console ID but preserves the session key and metrics association.
- Closing and reopening a console does not change session counters or KPIs.
- A detached running console can be listed, replayed through bounded scrollback, and reattached by
  `SessionKey` without starting a second PTY for the same console.
- Terminal output is delivered as base64 bytes, replay occurs before live output, and console exit is
  delivered once.
- A new console remains unkeyed until discovery returns its complete session ID.
- Missing `cwd` maps to `__no_directory__` and satisfies the project foreign key.
- `SessionRowDto` joins status by complete `SessionKey`, never by `sid` alone.
- Pending, ready, unavailable, null KPI values, and numeric zero serialize distinctly.
- Project KPIs equal formulas over summed counters, not averages of session KPIs.
- Provider errors are isolated: one adapter failure does not erase other providers' rows.
- API DTO serialization matches the examples and nullability in `apis.md`.
- Secret-bearing fixtures prove tokens and raw credential values are absent from all DTOs and errors.

## 11. Relationship to the other contracts

| Document | Defines |
|---|---|
| `urds.md` | User needs and product behavior |
| `frds.md` | Provider reading rules, functional requirements, and acceptance tests |
| `data.md` | Three current-state relational tables |
| `../atlases/data-model-atlas.json` / `.html` | Machine-readable and visual relational schema |
| `objects.md` | Rust objects, ownership, mapping, and lifecycle |
| `apis.md` | Tauri command/event DTOs consumed by React |
