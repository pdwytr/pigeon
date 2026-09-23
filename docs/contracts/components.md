# Pigeon — React Component Model Contract

**Version:** 0.1 · **Date:** 2026-09-13 · **Status:** implementation contract

This document defines the React component layer between the Tauri API contract and the functional
mockups. It describes the component tree, props, local state, query/event ownership, rendering
states, selection rules, and terminal behavior.

The component layer is intentionally a view model, not a second domain model. It may combine API
DTOs for presentation, but it must not parse provider output, calculate metrics, identify sessions by
`sid` alone, or own PTY processes.

The primary reference screens are:

- [`01-dashboard.html`](../UI/01-dashboard.html): projects-first dashboard, Live/Recent
  scope, project/session selection, project/session metrics, same-pane terminal.
- [`02-live-hover.html`](../UI/02-live-hover.html): compact live activity surface with
  bounded scrolling and compact mode.

The component model consumes [`apis.md`](apis.md) and the Rust-side objects in
[`objects.md`](objects.md).

## 1. Boundary and responsibility

```text
Tauri API commands/events
        ↓
api client + subscriptions
        ↓
normalized React view store
        ↓
components and selectors
        ↓
terminal renderer / DOM
```

Rust owns provider discovery, normalization, project aggregation, metric calculation, process
attribution, PTYs, terminal bytes, scrollback, and safe errors. React owns rendering, local
selection, scope tabs, project expansion, hover visibility, compact presentation, terminal screen
rendering, and user interaction dispatch.

### React must not own

- Provider file parsing or provider-specific response interpretation.
- Metric or KPI formulas.
- Session identity generation.
- Process discovery, PID matching, process termination, or PTY management.
- Durable session, project, or metric records.
- The authoritative terminal scrollback buffer.

### React must own

- Which project and session are selected.
- Whether a project’s session group is expanded.
- Whether the current scope is Live or Recent.
- Which console is visible in the detail pane.
- Terminal renderer creation, writing, fitting, and visible viewport position.
- Local optimistic interaction state such as “opening,” “stopping,” or “refreshing.”

## 2. Component tree

The production tree should remain close to this shape. Components may be split into files differently,
but ownership should remain the same.

```text
App
├── AppShell
│   ├── Topbar
│   │   ├── RefreshButton
│   │   ├── HoverToggleButton
│   │   └── LastUpdatedLabel
│   ├── WorkspaceSidebar
│   │   ├── WorkspaceHeading
│   │   ├── ScopeTabs
│   │   ├── NewProjectForm
│   │   ├── ProjectList
│   │   │   └── ProjectCard × N
│   │   │       ├── ProjectHeader
│   │   │       ├── ProjectStats
│   │   │       ├── ProjectSessionList
│   │   │       │   └── SessionListRow × N
│   │   │       └── ProjectActions
│   │   └── RecentSessionList [Recent scope]
│   │       └── SessionListRow × N
│   └── DetailPane
│       ├── DetailHeader
│       ├── ProjectSummaryRail [project selection]
│       ├── SessionSummaryRail [session selection]
│       ├── ProjectSessionSection
│       │   └── SessionListRow × N
│       ├── StatusSection
│       ├── MetricsSection
│       ├── UsefulnessSection
│       ├── CoverageSection
│       ├── SessionActionsSection
│       └── TerminalPanel [when a console is visible]
└── LiveHover [separate compact surface]
    ├── HoverHeader
    ├── HoverSummaryRail
    ├── HoverSessionList [bounded scroll]
    │   └── HoverSessionRow × N
    └── HoverFooter
```

`ProjectSessionList` and `RecentSessionList` use the same session-row view component with different
context props. The detail pane does not become a new route when selection changes. Project and
session metrics occupy the same local detail area, and the terminal is inserted below the selected
session’s metrics in that area.

## 3. View state model

The component layer has one normalized store and a small amount of presentation state. It should not
copy entire API responses into unrelated component states.

```ts
type Scope = "live" | "recent";
type Selection =
  | { kind: "project"; project: string }
  | { kind: "session"; key: SessionKey }
  | { kind: "none" };

interface ViewState {
  scope: Scope;
  selection: Selection;
  expandedProjects: Set<string>;
  sessions: {
    live: SessionRow[];
    recent: SessionRow[];
    loading: Record<Scope, boolean>;
    error: Record<Scope, EngineError[] | null>;
    generatedAtMs: Record<Scope, number | null>;
  };
  projects: {
    live: ProjectSummary[];
    recent: ProjectSummary[];
    loading: Record<Scope, boolean>;
    error: Record<Scope, EngineError[] | null>;
    generatedAtMs: Record<Scope, number | null>;
  };
  metrics: Record<string, MetricState>;
  status: StatusSnapshot | null;
  consoles: Record<string, ConsoleSummary>;
  visibleConsoleId: string | null;
  consoleUi: Record<string, ConsoleUiState>;
  lastUpdatedMs: number | null;
  refreshing: boolean;
  notices: Notice[];
}

interface ConsoleUiState {
  attached: boolean;
  rendererReady: boolean;
  replaying: boolean;
  fit: { cols: number; rows: number } | null;
  localScrollTop: number;
  inputEnabled: boolean;
}
```

The `metrics` map is keyed by a canonical serialized `SessionKey`, for example
`claude-code:<complete-sid>`. It must use the same key function everywhere:

```ts
sessionKeyId(key) = `${key.providerId}:${key.sid}`;
```

No component may use the display title, project name, truncated sid, array index, or `sid` alone as
the identity of a session or console.

### 3.1 Selection rules

- Selecting a project sets `{ kind: "project", project }` and renders project metrics.
- Selecting a session sets `{ kind: "session", key }` and renders session metrics while retaining
  the project as context.
- Selection changes do not change the API scope.
- A selected session that disappears from the current scope remains selected until the refresh result
  proves the session is invalid; the detail pane then shows “No longer in this view” with a way to
  switch scope or clear selection.
- A session switch detaches the visible terminal UI but does not call `console_close`.
- Reselecting a session reuses its existing console when one is listed for that SessionKey.
- Switching from Recent to Live never invents a live status for a closed session.

### 3.2 Project expansion rules

- Expansion is local UI state keyed by normalized project key.
- Live project cards default expanded, matching the mockup.
- Recent project cards preserve the user’s expansion state across refresh.
- Expansion does not fetch another API resource; the scoped session list already supplies rows.
- If a project has no rows in the selected scope, it is not rendered.

## 4. API client and event store

The API client is the only React module allowed to call `invoke` or subscribe to Tauri events.
Components dispatch intent to application actions; they do not call Tauri directly.

```ts
interface PigeonApi {
  hostInfo(): Promise<HostInfo>;
  settingsGet(): Promise<Settings>;
  settingsSet(patch: Partial<Settings>): Promise<Settings>;
  sessionsList(args: { scope: Scope; force?: boolean }): Promise<SessionListResult>;
  projectsSummary(args: { scope: Scope }): Promise<ProjectSummaryResult>;
  sessionMetrics(args: { key: SessionKey }): Promise<MetricState>;
  statusSnapshot(): Promise<StatusSnapshot>;
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
}
```

### 4.1 Startup

The application starts these calls in parallel:

```text
host_info
settings_get
sessions_list({ scope: "live" })
projects_summary({ scope: "live" })
status_snapshot
console_list
```

The latest dashboard mockup does not render the account/capacity strip, so `account_status` is not a
startup dependency for this component contract. It may be added later without changing the project,
session, or terminal component boundaries.

Startup rules:

1. Render the shell immediately.
2. Render loading placeholders in the sidebar and detail pane.
3. Render project/session rows as soon as list data arrives.
4. Render project metrics only after project summary data exists.
5. Lazily request selected-session metrics when a session is selected.
6. Reconcile consoles from `console_list` before deciding whether the selected console is attached.
7. Begin status refresh/subscriptions after the initial snapshot is accepted.

### 4.2 Refresh

The topbar refresh button dispatches:

```text
sessions_list({ scope: currentScope, force: true })
projects_summary({ scope: currentScope })
status_snapshot()
console_list()
```

Refresh does not clear existing rows, metrics, selection, or visible terminal output. It marks the
affected areas as refreshing and replaces them atomically when the responses arrive.

### 4.3 Events

The store subscribes to:

- `sessions://changed`: refetch the indicated scope and its project summaries.
- `sessions://metrics`: merge each metric state by complete SessionKey.
- `status://changed`: replace the status snapshot atomically and rejoin statuses by SessionKey.
- `console://data`: route bytes by console id to the terminal renderer.
- `console://exit`: update the console state and show an exit notice if it is visible.
- `pigeon://select-session`: select and scroll to the exact session row.

The store must ignore late responses for an older scope request. Each request gets a monotonically
increasing request token; only the newest token for that scope may replace the scope data.

## 5. Component contracts

The following contracts describe the minimum public interface. Internal styling props are intentionally
omitted.

### 5.1 AppShell

```ts
interface AppShellProps {
  api: PigeonApi;
  terminalFactory: TerminalFactory;
}
```

Responsibilities:

- Create the API client and view store.
- Start startup loading and event subscriptions.
- Own global notices and request errors.
- Render `Topbar`, `WorkspaceSidebar`, `DetailPane`, and `LiveHover`.
- Dispose event listeners and terminal instances on unmount.

It must not contain provider logic or metric formulas.

### 5.2 Topbar

```ts
interface TopbarProps {
  lastUpdatedMs: number | null;
  refreshing: boolean;
  hoverVisible: boolean;
  onRefresh(): void;
  onToggleHover(): void;
}
```

The refresh button is disabled only while a refresh is actively being started; repeated refreshes are
otherwise safe. `lastUpdatedMs` is formatted locally. “Show hover” invokes `hover_toggle`, and the
button reflects the returned visibility.

### 5.3 ScopeTabs

```ts
interface ScopeTabsProps {
  value: Scope;
  loading: boolean;
  onChange(scope: Scope): void;
}
```

The tabs are a mutually exclusive tablist:

- `Live`: provider terminals/processes open anywhere now.
- `Recent`: closed sessions from the last seven days.

Changing scope does not mutate source data. The selected project may remain selected if it exists in
the new scope; otherwise the detail pane changes to the first available project or an empty state.

### 5.4 ProjectList and ProjectCard

```ts
interface ProjectListProps {
  scope: Scope;
  projects: ProjectSummary[];
  sessions: SessionRow[];
  selectedProject: string | null;
  selectedSession: SessionKey | null;
  expandedProjects: Set<string>;
  onSelectProject(project: string): void;
  onToggleExpanded(project: string): void;
  onSelectSession(key: SessionKey): void;
  onOpenFolder(cwd: string): void;
  onAddSession(project: ProjectSummary, provider: ProviderId): void;
}
```

Each `ProjectCard` displays:

- `projectName`
- shortened `cwd` path with the full path available to assistive text/title
- live status dot when the project has live sessions
- scoped session count
- scoped token total when available
- expanded session rows
- Open project/folder action
- Add session engine menu

The project status dot is derived from `statusCounts`, not from whether a Pigeon console is attached.
For Live scope, a project is present only when it has at least one live session. For Recent scope, the
card represents closed sessions and must not show them as live.

### 5.5 SessionListRow

```ts
interface SessionListRowProps {
  row: SessionRow;
  selected: boolean;
  scope: Scope;
  consoleAttached: boolean;
  onSelect(key: SessionKey): void;
}
```

The row displays:

- provider badge
- session title/name with safe fallback to “Untitled session”
- project context when shown in the all-session list
- status badge when `status` is present
- “—” or equivalent neutral display when status is absent
- relative activity/closed time
- no truncated sid as an identity or primary label

The row key is `sessionKeyId(row.key)`. Clicking the row selects it and does not start or stop a
terminal.

### 5.6 NewProjectForm

```ts
interface NewProjectFormProps {
  initialPath: string;
  busy: boolean;
  onCancel(): void;
  onOpenProject(cwd: string): void;
}
```

The form accepts a folder path consistent with the Demo Studio-style project workflow. Submitting
it validates that the path is non-empty, then calls the native folder/project action. It does not
create a database project row directly. Project discovery creates or updates the project projection.

If the native picker is the source of truth, the visible text input is a display/entry convenience;
the final `cwd` still goes through Rust validation.

### 5.7 DetailPane

```ts
interface DetailPaneProps {
  selection: Selection;
  project: ProjectSummary | null;
  projectSessions: SessionRow[];
  selectedSession: SessionRow | null;
  selectedMetrics: MetricState | null;
  liveStatus: LiveSessionState | null;
  visibleConsole: ConsoleViewModel | null;
  onSelectSession(key: SessionKey): void;
  onOpenFolder(cwd: string): void;
  onAddSession(project: ProjectSummary, provider: ProviderId): void;
  onResume(session: SessionRow): void;
  onStop(session: SessionRow): void;
  onCloseConsole(consoleId: string): void;
}
```

The detail pane has two modes in the same location:

#### Project selection

Displays:

- eyebrow: `Project metrics · live activity` or Recent equivalent
- project name and cwd
- scoped session count and live count where applicable
- Open folder and Add session actions
- project summary rail
- project session list
- project totals and KPIs
- metrics coverage notice

#### Session selection

Displays:

- eyebrow: `Session metrics · <status>`
- session title/name
- project name and provider context
- Open folder, Stop session where applicable, and Resume in terminal
- session status/evidence
- session metrics
- session KPIs
- session actions
- `TerminalPanel` below the metrics when visible

The pane must not navigate to a separate project page, session page, or terminal browser tab.

### 5.8 MetricsSection

```ts
interface MetricsSectionProps {
  scope: "project" | "session";
  state: MetricState | null;
  totals?: MetricsTotals;
}
```

Rendering rules:

| State | Render | Never do |
|---|---|---|
| Missing | “Metrics not loaded” or request placeholder | show zero as a default |
| `pending` | “Counting…” | show blank cards that look final |
| `ready` | counters and derived KPIs | recalculate formulas in React |
| `unavailable` | safe error message and retry affordance | replace unavailable values with zero |

Project KPIs come from `ProjectSummary.kpis`. Session KPIs come from the selected session’s
`MetricState`. The component only formats numbers and labels.

### 5.9 StatusSection

```ts
interface StatusSectionProps {
  row: SessionRow;
  observation: LiveSessionState | null;
}
```

It renders status, “in state” duration, provider evidence, PID only when appropriate for the UI, and
safe diagnostics. A closed Recent session may show `finished` or no active status, but a Live session
must have an active process observation.

### 5.10 ProjectActions and SessionActions

Project actions:

- Open folder → `folder_open({ cwd })`.
- Add session/provider selection → `session_start({ provider, cwd, cols, rows })`.
- New project → project selection flow; no direct database mutation.

Session actions:

- Resume → `console_open({ sessionKey, cwd, cols, rows })`.
- Stop → `session_stop({ key })`.
- Close terminal → `console_close({ id })`.
- Refresh metrics → `session_metrics({ key })`.

Action buttons expose busy and failure states. Stop is not available for a session without a proven
live process. Resume is disabled when `resumable` is false and explains `resumeBlockedReason`.

## 6. Terminal component contract

Terminal display is a component-layer responsibility, but terminal execution is a Rust API/object
responsibility. The split is deliberate:

```text
ConsoleService (Rust)
  owns PTY, process, bytes, resize, scrollback, lifecycle
        ↓ Tauri commands/events
TerminalController (React)
  owns attachment, subscriptions, replay ordering, renderer lifecycle
        ↓ imperative adapter
TerminalRenderer (React-facing xterm-like object)
  owns screen, cursor, ANSI rendering, selection, viewport
```

### 6.1 Terminal types

```ts
interface ConsoleViewModel {
  summary: ConsoleSummary;
  attached: boolean;
  rendererReady: boolean;
  replaying: boolean;
  fit: { cols: number; rows: number } | null;
}

interface TerminalFactory {
  create(container: HTMLElement): TerminalRenderer;
}

interface TerminalRenderer {
  open(container: HTMLElement): void;
  write(data: Uint8Array): void;
  clear(): void;
  resize(cols: number, rows: number): void;
  focus(): void;
  dispose(): void;
  onData(handler: (data: Uint8Array) => void): () => void;
  onResize(handler: (size: { cols: number; rows: number }) => void): () => void;
}
```

The renderer may be xterm.js or an equivalent implementation. The component contract does not force
the library, but it requires byte writes, resize notifications, focus, disposal, and user input.

### 6.2 TerminalPanel props

```ts
interface TerminalPanelProps {
  console: ConsoleViewModel;
  api: Pick<PigeonApi,
    "consoleReady" | "consoleInput" | "consoleResize" |
    "consoleClose" | "consoleScrollback">;
  terminalFactory: TerminalFactory;
  onClose(consoleId: string): void;
}
```

The panel displays:

- provider CLI label
- resume/new mode
- working directory
- PTY state: starting, running, exited, or closed
- terminal viewport
- bounded scrollback/replay indicator while attaching
- close terminal action
- exit code or safe exit notice when applicable

It does not display a second page, open a browser tab, or create a second session detail route.

### 6.3 Terminal attach sequence

For a newly opened console:

```text
1. console_open(sessionKey, cwd, measured cols/rows)
2. receive console id
3. register console://data and console://exit handlers
4. create and mount TerminalRenderer
5. measure container and call console_resize
6. call console_ready only after renderer is mounted and subscriptions exist
7. write subsequent console://data bytes to the renderer
```

For an existing detached console:

```text
1. select the session
2. find ConsoleSummary by SessionKey
3. create/mount renderer
4. subscribe to console data before replay
5. call console_scrollback(id)
6. write replay bytes exactly once
7. continue writing live bytes
8. call console_ready
```

The replay/live ordering must prevent both lost bytes and duplicated bytes. The Rust console service
is responsible for making the stream boundary safe; React must not call `console_scrollback` more than
once per attach attempt.

### 6.4 Terminal input

```text
renderer.onData(bytes)
  → base64 encode bytes
  → console_input({ id, dataB64 })
```

Input is sent only while the console is `starting` or `running` and the panel is attached. Input after
`exited` or `closed` is rejected locally and by Rust. React must preserve bytes exactly; it must not
normalize newlines or interpret provider commands.

### 6.5 Terminal resize

Resize is driven by the terminal container, not by arbitrary window dimensions:

```text
ResizeObserver(container)
  → terminal renderer fit()
  → console_resize({ id, cols, rows })
```

Rules:

- Ignore zero or non-integer dimensions.
- Debounce rapid resize events.
- Send the final measured size after attach and after pane/layout changes.
- Keep the terminal in the same detail pane when the selected session changes.
- A resize failure becomes a visible but non-blocking notice; it must not destroy the renderer.

### 6.6 Terminal detach and close

Detaching the visible terminal means removing or hiding the React renderer while retaining the Rust
console. It does not call `console_close`.

`console_close` is called only when:

- the user explicitly closes the terminal;
- the application is shutting down; or
- the console has already exited and Rust accepts an idempotent cleanup request.

When a console exits:

1. stop accepting input;
2. flush any already received bytes;
3. show the exit state/code;
4. keep the final output visible until the user closes or switches away;
5. leave the Session and its metrics unchanged;
6. let status/session refresh determine whether the provider session entered Recent.

### 6.7 Terminal failure states

The panel must distinguish:

- `NOT_FOUND`: console disappeared; remove the attachment and refresh `console_list`.
- `NOT_READY`: keep the panel in starting state with retry/diagnostic text.
- PTY exit: show final output and exit code.
- Resize failure: keep output visible and show a small non-blocking notice.
- Scrollback truncation: show that only bounded recent output was restored.
- Renderer failure: show a recoverable “Terminal unavailable” state without changing session data.

## 7. Live hover component

```ts
interface LiveHoverProps {
  visible: boolean;
  status: StatusSnapshot | null;
  sessions: SessionRow[];
  compact: boolean;
  onCompactChange(compact: boolean): void;
  onClose(): void;
  onSelect(key: SessionKey): void;
}
```

The hover is a projection of Live data only. It does not perform a second provider scan.

The current mockup requires:

- small maximum width/height;
- bounded session-list height with vertical scrolling;
- compact mode that reduces metadata and height;
- primary row text that leads with project context, followed by the session name;
- supporting row text that leads with the provider badge, followed by recency when shown;
- click-through selection using the complete SessionKey;
- summary counts derived from the same `StatusSnapshot` as the dashboard.

The hover must not show closed Recent sessions. If there are more rows than fit, scrolling is used;
the component may show a “more below” hint but must not fabricate counts.

`onSelect` dispatches `hover_select(key)`, then the main window handles
`pigeon://select-session` by selecting and scrolling to the exact row.

## 8. Loading, empty, and error states

Every data-bearing component has a defined state. No component should silently render an empty array
for a failed request.

| Surface | Loading | Empty | Error/partial |
|---|---|---|---|
| Live project list | project skeleton cards | “No live projects” | scoped provider problems notice |
| Recent project list | skeleton cards | “No sessions closed in the last 7 days” | provider-specific notice |
| Project detail | project placeholder | “Select a project” | partial totals + coverage notice |
| Session detail | metadata plus metric pending | “Select a session” | unavailable metric state |
| Session row | title placeholder | not rendered | safe row-level status/problem |
| Terminal | connecting | no console | PTY/renderer/replay error |
| Hover | compact skeleton | “No live sessions” | stale/status notice |

Partial provider failure must preserve successful providers. A failed OpenCode read, for example,
must not remove Claude or Codex rows.

## 9. Accessibility and keyboard behavior

- Scope tabs use `role="tablist"`, `role="tab"`, `aria-selected`, and a controlled tab panel.
- Project expansion controls use `aria-expanded` and an accessible project-specific label.
- Project/session rows are buttons or keyboard-equivalent controls, not clickable non-semantic divs.
- Selected project/session rows expose `aria-current` or an equivalent selected state.
- Status uses text and color; color alone never communicates running/needs-you/finished.
- Terminal output has an accessible label containing provider, project, and session name.
- Terminal input focus is explicit and does not steal focus when background output arrives.
- The hover close and compact controls have labels and pressed state.
- Keyboard selection from the hover must focus the main window and reveal the selected row.
- The terminal must remain usable without requiring pointer-only resizing or scrolling.

## 10. Data formatting rules

Formatting is presentation-only:

- Convert UTC epoch milliseconds to local relative times.
- Format token counts consistently across project cards, rails, and metric cards.
- Format nullable numbers as `—` or an explicit unavailable label, never `0`.
- Format `providerCostUsd` only when non-null.
- Truncate paths visually with the full `cwd` available to tooltip/assistive text.
- Keep full `sid` out of ordinary visual labels unless a diagnostics view explicitly requests it.
- Display provider IDs as friendly labels (`CLAUDE`, `CODEX`, `OPENCODE`) without changing the API
  value used for commands or keys.

## 11. Interaction-to-API matrix

| UI interaction | Component owner | API call/event | Result |
|---|---|---|---|
| Startup | AppShell | startup calls + subscriptions | render Live scope |
| Refresh | Topbar/AppShell | forced list, summary, status, console list | replace current data atomically |
| Live tab | ScopeTabs | list/summary with `live` | active terminals/processes only |
| Recent tab | ScopeTabs | list/summary with `recent` | closed last-seven-day sessions |
| Project click | ProjectCard | local selection | project metrics in detail pane |
| Session click | SessionListRow | local selection + `session_metrics` | session metrics in same detail pane |
| Expand project | ProjectCard | local state | show/hide grouped rows |
| Open folder | ProjectActions | `folder_open` | host opens cwd |
| Add session | ProjectActions | `session_start` | new console, then discovery |
| Resume | SessionActions | `console_open` | embedded terminal below metrics |
| Stop | SessionActions | `session_stop` | refresh scoped rows/status |
| Close terminal | TerminalPanel | `console_close` | close PTY, preserve session |
| Terminal output | TerminalController | `console://data` | renderer writes bytes |
| Terminal exit | TerminalController | `console://exit` | show exit state |
| Session switch | DetailPane | local detach, no close | preserve console for reattach |
| Hover row click | LiveHover | `hover_select` / selection event | focus main and select row |
| Compact hover | LiveHover | local state/settings | resize presentation only |

## 12. Invariants and acceptance tests

### Session and project

- Project click always shows project totals/KPIs, never session metrics.
- Session click always shows session metrics, never a separate project page.
- Every rendered session row can be traced to a complete `SessionKey`.
- A project card’s scoped session count equals the rows rendered under it.
- Live cards contain only sessions with active provider processes/terminals.
- Recent cards contain only closed sessions within the configured seven-day window.
- Status counts and hover counts equal the rows in the canonical live snapshot.

### Metrics

- Pending metrics visibly say counting/loading and never look like zero.
- Unavailable metrics preserve the error state and keep other provider/project data visible.
- React never computes context-per-call, rewrite ratio, or batching ratio.
- Reopening a session leaves all displayed counters and KPIs associated with the same SessionKey.

### Terminal

- Resume creates a console in the current detail pane, not a new tab or route.
- The terminal appears below the selected session metrics.
- Terminal bytes are written in event order and are not parsed as provider records by React.
- A detached console remains running when another session is selected.
- Returning to a session reuses the console and restores bounded scrollback once.
- Closing a console does not delete the session or reset metrics.
- Terminal input and resize commands use the current console id, never the session sid.
- PTY exit leaves final output visible and does not erase session metrics.
- All terminal instances and event subscriptions are disposed on app shutdown/unmount.

## 13. File/module recommendation

```text
src/
  api/
    pigeonApi.ts          // generated binding wrapper; invoke/subscription only
    types.ts               // generated API types, not hand-edited
  store/
    viewStore.ts            // normalized ViewState and reducers/actions
    selectors.ts            // project/session/console selectors
    subscriptions.ts        // Tauri event subscriptions
  components/
    AppShell.tsx
    Topbar.tsx
    WorkspaceSidebar.tsx
    ScopeTabs.tsx
    ProjectCard.tsx
    SessionListRow.tsx
    DetailPane.tsx
    MetricsSection.tsx
    StatusSection.tsx
    LiveHover.tsx
    terminal/
      TerminalPanel.tsx
      TerminalController.ts
      TerminalRenderer.ts
      terminalEncoding.ts
  format/
    numbers.ts
    times.ts
    paths.ts
    providerLabels.ts
```

The important boundary is `TerminalController`: it translates Rust console events and commands into
renderer operations. `TerminalPanel` renders the shell around it. No terminal implementation should
be embedded inside `SessionListRow`, `DetailPane`, or the API client.

## 14. Relationship to the other contracts

| Contract | Component dependency |
|---|---|
| `urds.md` | user goals and product behavior |
| `frds.md` | provider/source behavior and acceptance requirements |
| `data.md` | durable relational projections |
| `objects.md` | Rust domain objects, runtime console, lifecycle |
| `apis.md` | commands, events, DTOs, terminal byte protocol |
| `COMPONENT-MODEL-CONTRACT.md` | React composition, state, rendering, and interaction |
| functional mockups | visual reference for the component composition |

