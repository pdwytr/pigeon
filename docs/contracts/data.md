# Pigeon — Relational Data Model

**Version:** 0.7 · **Date:** 2026-09-12 · **Companions:** `../atlases/data-model-atlas.html` and
`../atlases/data-model-atlas.json`

Pigeon's persistent/current-state model has exactly three tables:

```text
accounts  1 ───────── N  sessions  N ───────── 1  projects
```

The model is latest-state only. It is not an event log, metric history, poll history, or analytics
warehouse. The first implementation may keep these rows in memory; if SQLite is introduced, it uses
this same three-table shape.

## `accounts`

One current account row per installed engine account. Pigeon stores safe identity and the latest
capacity values together because the account strip consumes them together.

```sql
accounts(
  account_id TEXT PRIMARY KEY,
  provider_id TEXT NOT NULL UNIQUE CHECK (provider_id IN ('claude-code', 'codex', 'opencode')),
  label TEXT,
  organization TEXT,
  plan TEXT,
  tier TEXT,
  mode TEXT,
  account_short TEXT,
  signed_in INTEGER NOT NULL,
  capacity_supported INTEGER NOT NULL,
  five_hour_used_pct REAL,
  five_hour_resets_at_ms INTEGER,
  five_hour_stale INTEGER NOT NULL,
  weekly_used_pct REAL,
  weekly_resets_at_ms INTEGER,
  weekly_stale INTEGER NOT NULL,
  reached_limit TEXT,
  provider_summary TEXT,
  read_at_ms INTEGER NOT NULL,
  problem_kind TEXT,
  problem_detail TEXT
)
```

`provider_summary` is a bounded display projection such as provider names/types, not raw JSON and
never a key or token. Capacity is intentionally not a child table: Pigeon has exactly two displayed
windows, five-hour and weekly.

## `projects`

One current row per normalized working directory represented by the current session inventory.
Projects are rebuilt/upserted from sessions; they are not a historical project registry.

```sql
projects(
  project_id TEXT PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,
  leaf TEXT NOT NULL,
  last_active_ms INTEGER NOT NULL,
  session_count INTEGER NOT NULL,
  live_session_count INTEGER NOT NULL
)
```

Projects with no qualifying session are omitted from the selected Live or Recent view. Project totals
and KPIs are derived from the related sessions, not stored as competing copies.

## `sessions`

One current row per logical engine session. This table carries the session metadata, current metrics,
and current live state because all three are displayed together and have the same session identity.

```sql
sessions(
  provider_id TEXT NOT NULL CHECK (provider_id IN ('claude-code', 'codex', 'opencode')),
  sid TEXT NOT NULL,
  account_id TEXT,
  project_id TEXT NOT NULL,

  cwd TEXT,
  title TEXT NOT NULL,
  name TEXT,
  git_branch TEXT,
  first_active_ms INTEGER,
  last_active_ms INTEGER NOT NULL,
  resumable INTEGER NOT NULL,
  resume_blocked_reason TEXT,
  source_summary TEXT,
  unknown_types TEXT,

  metrics_state TEXT NOT NULL CHECK (metrics_state IN ('pending', 'ready', 'unavailable')),
  input_tokens INTEGER,
  output_tokens INTEGER,
  cache_read INTEGER,
  cache_write INTEGER,
  api_calls INTEGER,
  tool_calls INTEGER,
  user_turns INTEGER,
  duration_ms INTEGER,
  reasoning_tokens INTEGER,
  engine_cost_usd REAL,
  metrics_error_kind TEXT,
  metrics_error_detail TEXT,

  process_present INTEGER NOT NULL,
  live_state TEXT CHECK (live_state IN ('running', 'needs_you', 'finished', 'unknown')),
  live_since_ms INTEGER,
  live_raw_word TEXT,
  live_pid INTEGER,
  live_console_id TEXT,
  live_evidence TEXT,
  live_observed_at_ms INTEGER,

  PRIMARY KEY (provider_id, sid),
  FOREIGN KEY (account_id) REFERENCES accounts(account_id) ON DELETE SET NULL,
  FOREIGN KEY (project_id) REFERENCES projects(project_id) ON DELETE CASCADE
)
```

Every session has a project FK. If the provider emits no working directory, Pigeon assigns the
reserved current project row:

```text
project_id = "__no_directory__"
path       = "(no directory)"
leaf       = "(no directory)"
```

`account_id` is populated by looking up the unique current `accounts` row whose `provider_id` matches
the session's `provider_id`. If the provider account cannot be identified, it remains null.

`source_summary`, `unknown_types`, `metrics_error_detail`, and `live_evidence` are bounded display
projections, not raw provider documents. If they need structured rendering later, they can be promoted
to child tables; they do not justify adding tables to the first pass.

The three KPIs are deliberately absent from storage. Rust ports the frozen Studio formulas and
calculates them when producing `SessionRow`, `ProjectSummary`, or a detail response:

```text
context_per_call = cache_read / api_calls
rewrite_ratio    = cache_write / cache_read
batching_ratio   = tool_calls / api_calls
```

A zero denominator produces `NULL`, never zero. Project KPIs are calculated after summing the current
session counters; session KPIs are never averaged.

## Provider-neutral boundary

`provider_id` is only an identity namespace used to select the correct adapter and account. It does
not authorize provider-specific columns in `sessions`; Claude, Codex, and OpenCode fields are mapped
into the same shared columns.

## What is deliberately not a table

These are runtime mechanisms, not persisted product data:

- `ProviderId` registry and adapter metadata: compile-time Rust configuration.
- Console registry: in-memory PTY state keyed by `console_id`; a new console may exist before its
  session is discovered.
- Terminal scrollback: bounded in-memory byte ring, replayed when the user returns to a session.
- Source signatures, poll deadlines, in-flight tasks, and cache entries.
- SQL views for SessionRow, Live projects, Recent projects, StatusSnapshot, and ConsoleSummary.

Persisting console bytes or every poll would create high-volume data with no value after Pigeon exits.

The `SessionRow` view joins `sessions` to `projects` on `project_id`. Live project summaries and
status snapshots are derived from the current session rows; no separate summary table is required.

## Configuration

Pigeon configuration is stored in a TOML file:

```text
<Tauri app-data>/config.toml
```

It contains the latest hover settings, selected view, pane split, `pollIntervalSeconds` (default 5,
bounds 1–300), and `recentWindowDays` (default 7, bounds 1–365). It contains no credentials,
transcripts, metrics history, or process history.

```toml
[view]
scope = "live"
split = 0.38

[polling]
interval_seconds = 5
recent_window_days = 7

[hover]
visible = false
corner = "tr"
x = 0
y = 0
```

## Credentials

Pigeon follows Demo Studio's existing readers:

- Claude identity: `~/.claude.json` `oauthAccount` safe fields.
- Claude capacity: `~/.claude/.credentials.json`, then macOS Keychain service
  `Claude Code-credentials` via `security find-generic-password -s "Claude Code-credentials" -w`.
- Codex identity: `~/.codex/auth.json`, only `auth_mode` and `tokens.account_id`.
- Codex capacity: rate limits from the newest rollout tail; never run Codex to refresh it.
- OpenCode identity: provider names/types from `~/.local/share/opencode/auth.json` and only the
  `email` column from its `account` table.

Tokens, refresh tokens, API keys, cookies, headers, raw credential blobs, and Keychain output never
enter `accounts`, settings, logs, API payloads, or the WebView.

## Write policy

- Polling compares source signatures and semantic state in memory first.
- Unchanged polls produce zero table writes.
- A changed session rewrites one current `sessions` row and, if needed, one current project row.
- Metrics replace the current raw-counter columns only when the source signature changes.
- KPI values are calculated in code at read/projection time and never stored.
- Account/capacity fields replace only on refresh or changed values.
- No historical rows are retained.

## Reuse from Demo Studio

Pigeon reuses Studio's proven Claude counting fold, frozen KPI formulas, engine discovery/title rules,
capacity fixtures, PTY locking/attach order/chunking/shutdown, PATH/TERM setup, and redaction/error
patterns. Pigeon-specific work is limited to the Rust mappers, Live/Recent scopes, project-first
views, new-session launch, and session-wide process stop proof.
