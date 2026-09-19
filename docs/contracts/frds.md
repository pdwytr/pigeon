x# Pigeon — Functional Requirements Document (FRD)

**Status:** draft v0.2 for owner review · **Date:** 2026-09-13 · **Owner:** the owner ·
**Author:** Claude Fable 5.1 · **Companion:** `urds.md` (user requirements, `UR-##`), which every
functional requirement here traces to.

**Conventions.**
- `FR-##` functional, `NFR-##` non-functional, `T-##.#` acceptance test of `FR-##`, `OQ-#` open
  question. Ids are never renumbered or reused.
- **MUST / SHOULD / MAY** as in RFC 2119. A MUST that cannot be met stops the pass; it is not
  quietly downgraded.
- **Origin** (owner / agent / joint) follows the studio's `LORE.md` convention.
- **Provenance** names the demo-studio file and line range a rule was copied from, at branch
  `mac-first-launch-fixes-2026-09-11`. Pigeon copies; it never imports.
- **"This machine"** means the machine a fact was measured on, and the fact names it. Every measured
  fact in this document is from the owner's Mac (Apple silicon, macOS 15) on 2026-09-12 unless it
  says otherwise. Windows-specific facts cite the studio.
- Field names in `code` are the engine's own spellings and are contract: a renamed field is drift
  and is handled by FR-26, never by a guess.

---

## 1. System overview

### 1.1 Processes and zones

Pigeon is one Tauri v2 application: a Rust **host** process and a **WebView** running the React
view. There is no sidecar and no store. The host owns everything that touches the filesystem, the
network, processes and PTYs; the view owns rendering and user input. The two talk only through
Tauri commands (view → host, request/response) and Tauri events (host → view, one-way).

```
┌───────────────────────────────── host (Rust) ─────────────────────────────────┐
│ engines/   Claude · Codex · OpenCode adapters behind one trait (FR-1)          │
│ status/    process table + engine status files → live status (FR-19..22)      │
│ metrics/   frozen counting rules + the three KPIs + project rollup (FR-16..18)│
│ console/   PTY runtime, one Console per hosted session (FR-8..10)             │
│ cache/     typed TTL slots, refresh coalescing (FR-25)                        │
│ pathenv/   login-shell PATH + TERM for GUI launches (FR-28)                   │
│ settings/  hover position, list toggles (FR-32)                               │
│ log/       redacted JSON-lines log (FR-33)                                    │
└──────────────── commands ↑ (invoke)      events ↓ (emit) ─────────────────────┘
┌──────────────────────────────── view (React) ─────────────────────────────────┐
│ main window:  AccountStrip · StatusRail · SessionList/ProjectGroups · RightPane│
│ hover window: counters + up to 8 live rows (FR-23)                             │
└───────────────────────────────────────────────────────────────────────────────┘
```

### 1.2 What is read, and how often

| Source | Engine | Read for | Cadence |
|---|---|---|---|
| `~/.claude/projects/<enc>/<uuid>.jsonl` heads | Claude | rows | every `pollIntervalSeconds`, on demand |
| same files, full | Claude | counters (lazy) | once per `(path, size, mtime)` |
| `~/.claude/sessions/<pid>.json` | Claude | live status, session name | every `pollIntervalSeconds` |
| `~/.claude.json`, credential blob / Keychain | Claude | identity, plan words | every 5 min |
| `https://api.anthropic.com/api/oauth/usage` | Claude | capacity | every 15 min, on demand |
| `~/.codex/sessions/**/rollout-*.jsonl` heads | Codex | rows | every `pollIntervalSeconds` |
| same, full, all files of a thread | Codex | counters (lazy) | once per group signature |
| newest rollouts, tail | Codex | capacity | every 15 min |
| `~/.codex/session_index.jsonl` | Codex | names | every `pollIntervalSeconds` |
| `~/.codex/auth.json` | Codex | identity | every 5 min |
| process table (`sysinfo`), `lsof -p` on codex pids | Codex, OpenCode | live status | every `pollIntervalSeconds` |
| `~/.local/share/opencode/opencode.db` (WAL, read-only) | OpenCode | rows, counters, status, permissions | every `pollIntervalSeconds` |
| `~/.local/share/opencode/auth.json` | OpenCode | identity | every 5 min |
| pigeon's own console registry | all | hosted-session status | event-driven |

Nothing is written to any of these. Pigeon writes only its settings file and its log (FR-32,
FR-33).

`pollIntervalSeconds` defaults to 5 and is configurable within the FR-36 bounds. Polling compares
source signatures and semantic state in memory before touching current-state tables; unchanged polls
produce no writes. `recentWindowDays` controls the Recent view cutoff and defaults to 7.

### 1.3 Module map (host)

```
src-tauri/src/
  lib.rs            module registration, tauri::Builder, managed state, invoke handler list
  main.rs           two lines
  providers/mod.rs  ProviderId, Provider adapter trait, CapacitySource, MetricsSource, StatusSource,
                    EngineReport, EngineError, Secret
  engines/claude.rs engines/codex.rs engines/opencode.rs
  status.rs         process table, decision table, snapshot, poller
  metrics.rs        RawSix, Kpis, formulas, ProjectRollup
  console.rs        PTY runtime (copied from studio, stripped; see Appendix B)
  cache.rs          Cached<T>
  pathenv.rs        PATH/TERM bootstrap
  settings.rs       config.toml
  log.rs            JSON-lines log with the redaction guard
src/
  App.tsx  ipc.ts  hover/HoverApp.tsx
  components/  AccountStrip CapacityBar EngineCard StatusRail SessionRow ProjectHeader
               ProjectCard SessionDetail MetricChips KpiRow StatusBadge ConsoleView
  console/     ConsoleView terminalEngine consoleTheme          (copied)
  platform/    consoleTransport hostEvents                      (copied, stripped)
```

---

### 1.4 Reuse boundary: Demo Studio is prior art

Pigeon MUST reuse the proven Studio implementation and fixtures wherever the behavior is the same:

- Claude usage folding, message-id dedupe, elementwise-MAX handling, and frozen KPI formulas.
- Engine discovery/title extraction rules and redacted capacity fixtures.
- PTY registry locking, attach order, byte chunking, shutdown, and PATH/TERM launch behavior.
- Error catalogue, secret redaction, UTC handling, and boundary-test patterns.

Pigeon-specific work is limited to Rust provider mappers, Live/Recent project scoping, the native
folder picker/new-session launch, session-wide process identity/termination proof, and current-state
relational projections. A new parser, KPI definition, or terminal lifecycle rule requires evidence
that the Studio behavior cannot be reused.

## 2. Engine source contracts

Each subsection is the complete statement of what pigeon reads from one engine and how. It is
the adapter's specification and the drift ledger: when the engine changes, this section changes
with an ADR, and the code follows.

### 2.1 Claude Code (CLI 2.1.269 on this machine)

**2.1.1 Session files.** Root `~/.claude/projects/`. One directory per working directory, named by
an encoding of the absolute path (`/` → `-`, drive colon → `-`; e.g.
`-Users-khalid-Documents-Projects-demo-studio`). The encoding is **lossy** (a folder name
containing `-` is indistinguishable from nesting) and MUST NOT be decoded. Inside each directory:

| Entry | Meaning | Pigeon |
|---|---|---|
| `<uuid>.jsonl` at depth 1, stem matching `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$` | a session's main transcript | **a session** |
| `<uuid>/` directory | that session's sidecar tree: `subagents/agent-*.jsonl`, `tool-results/` | read by the counters fold only (§2.1.6) |
| `memory/` and any other directory | not sessions | ignored |
| `*.jsonl` with a non-UUID stem | not seen; would be drift | ignored, counted in `problems` |

Measured: 19 directories, 50 session files (16 KB – 14.6 MB), 45 subdirectories.

**2.1.2 Record envelope.** One JSON object per line. Top-level keys used: `type` (string),
`timestamp` (ISO-8601 UTC with `Z`), `sessionId`, `cwd`, `gitBranch`, `isMeta`, `isSidechain`,
`isCompactSummary`, `message` (object). Record types seen in the five newest files and their
handling:

| `type` | Count (5 files) | Pigeon uses it for |
|---|---|---|
| `user` | 552 | title, user turns, cwd (some carry `cwd`) |
| `assistant` | 933 | counters (usage, tool_use), duration |
| `attachment` | 948 | **cwd** (first carrier, by line 7 in 50/50 files) |
| `ai-title` | 180 | **title** — `{"type":"ai-title","aiTitle":"<text>","sessionId":…}`; the last one in the file wins |
| `last-prompt`, `mode`, `permission-mode`, `atis-latch` | ~185 each | nothing (control records) |
| `file-history-snapshot`, `file-history-delta` | 63, 4 | nothing |
| `system`, `queue-operation`, `agent-name`, `pr-link`, `cost-state` | 73, 50, 6, 30, 2 | nothing in the first pass |
| anything else | — | nothing; the type is added to a per-file `unknown_types` count surfaced in the detail pane (FR-26) |

A line that fails to parse as JSON is skipped and counted (a mid-write tail is normal; UR-11 A3).

**2.1.3 Head read (rows).** Read at most 64 KB or 200 lines, whichever first. Stop as soon as all of
`cwd`, `gitBranch` (optional), `first_ts` and a title candidate are known. Rules:
- `cwd` = the first record carrying a non-empty top-level `cwd` string.
- `first_ts` = the first record carrying `timestamp`.
- Title candidate = the first **user text** record (§2.1.4). If none within the head, the row's title
  is `""` and the view shows `(untitled)` until the fold (§2.1.6) supplies an `ai-title`.
- `last_active_ms` = max(mtime of the main file, mtime of every `<uuid>/subagents/*.jsonl`) — one
  `readdir` of the sidecar directory, no read. The studio measured a session that did all its work
  through a subagent and looked idle by its main file for 40+ minutes (`capture/watcher.py:48-64`).

**2.1.4 User text record.** A record is a user text record when all hold: `type == "user"`;
`message.role == "user"`; `isMeta` is not `true`; `isCompactSummary` is not `true`; `isSidechain` is
not `true`; and `message.content` is either a non-blank string, or a list containing at least one
`{type:"text", text}` part and **no** `{type:"tool_result"}` part. The text is the string, or the
`text` parts joined with a space; whitespace runs collapse to one space; leading XML-like blocks
(`<tag ...>…</tag>` at the very start) are removed before trimming; if what remains is empty the
record is not a title candidate but still counts as a user turn.
Provenance: `capture/session_meta.py:712-734` (the two exclusions and the extraction), extended by
the sidechain exclusion and the leading-tag strip (agent, 2026-09-12).

**2.1.5 Title precedence** for a Claude row: (1) the last `ai-title` record's `aiTitle` when the
fold has run; (2) the head's first user text, truncated to 200 characters; (3) `(untitled)`. A live
session's engine-given `name` (§2.1.8) is shown as a tag beside the title, never instead of it.

**2.1.6 Counters (the fold).** Reads the main transcript and every `<uuid>/subagents/*.jsonl`.
For each record with `type == "assistant"` and a string `message.id`:
- usage keys folded per id by **elementwise MAX**: `input_tokens`, `output_tokens`,
  `cache_creation_input_tokens`, `cache_read_input_tokens` (from `message.usage`). A missing key is
  0. `output_tokens` is a streaming counter that grows across the records of one id; MAX is the
  final value. Other usage keys (`cache_creation`, `service_tier`, `iterations`, `speed`,
  `inference_geo`, `server_tool_use`, `output_tokens_details`) are ignored.
- `tool_use` blocks: the set of `content[].id` where `content[].type == "tool_use"`, per id.
- **API calls** = number of distinct ids across all files read. **Tool calls** = size of the union
  of tool_use ids. A message id seen in both the main transcript and a subagent file is one call
  (the studio's 2026-09-11 ruling that a forked subagent restates the parent's call).
- **User turns** = user text records (§2.1.4) in the main transcript.
- **Duration** = last `timestamp` − first `timestamp` in the main transcript.
- The raw six are: `input_tokens` = Σ input, `output_tokens` = Σ output, `cache_read` = Σ
  cache_read_input_tokens, `cache_write` = Σ cache_creation_input_tokens, `api_calls`, `tool_calls`.
Provenance: `adapters/claude_code/parser.py` around line 1258 (`fold_usage_max`, `_build_api_calls`),
`planning/project-seed.md §2.3 rule 2`. Cross-checked by T-16.1 against the studio's figures.

**2.1.7 Identity.** `~/.claude.json` (note: **beside** `~/.claude/`, not inside it) →
`oauthAccount` object → `emailAddress`, `displayName`, `fullName`, `organizationName`,
`organizationType`, `seatTier`, `userRateLimitTier`, `organizationRateLimitTier`, `accountUuid`,
`organizationUuid`. Label = first non-blank of `emailAddress`, `displayName`, `fullName`,
`organizationName`. Plan words come from the credential blob (§2.1.9): `claudeAiOauth.
subscriptionType` (plan) and `claudeAiOauth.rateLimitTier` (tier). `accountUuid` is shown as its
first 8 characters. Absent block → "not signed in with a Claude subscription" (an API-key login has
no `oauthAccount` and no capacity windows).
Provenance: `capture/usage_capacity.py:1428-1468` (`_profile_account`), `:868-869`.

**2.1.8 Live status.** Two signals, the status file first and the transcript tail as corroboration. Directory `~/.claude/sessions/`. For each running CLI a file `<pid>.json`:

```json
{"pid":35742,"sessionId":"562dd77f-…","cwd":"/Users/khalid/Documents/Projects/demo-studio",
 "startedAt":1789218438483,"procStart":"Sat Sep 12 13:07:15 2026","version":"2.1.269",
 "kind":"interactive","entrypoint":"cli","name":"session-console-monitor","nameSource":"auto",
 "status":"busy","updatedAt":1789220994891,"statusUpdatedAt":1789220994891, …}
```
(also `peerProtocol`, `peerFeatures`, `pidDomain`, `messagingSocketPath`, `nameSince` — ignored.)
Beside each, a `<pid>.<hash>.key` file — **never read**. Rules: a file whose `pid` is not alive
(`kill(pid, 0)` fails with ESRCH) is a finished session and its `status` is ignored; a live pid's
`status` word maps per §6; `statusUpdatedAt` is the time-in-state origin; `name` is the session's
engine-given name. Measured: 4 live (1 `busy`, 3 `idle`), 182 ms for the equivalent `claude agents
--json` (fields `cwd, kind, name, pid, sessionId, startedAt, status`), whose documentation names
`needs_input` as "blocked and waiting for your response". Pigeon reads the files; the command is
the test oracle (T-19.3).
**Tail corroboration** (the studio's own rule, `adapters/claude_code/parser.py:2040-2181` and
`adapters/__init__.py:72-76`): when the file says `idle`, read the last ~64 KB of the transcript; if
the **terminal assistant message** carries a `tool_use` block with no matching `tool_result`
anywhere after it and the block's `name` is `AskUserQuestion` or `ExitPlanMode`, the session is
**needs you** (the CLI is showing the owner a question or a plan to approve), dated from the
block's own `ts`; otherwise `idle` is **finished**. An unanswered `tool_use` of any other name on
the terminal message with the file saying `busy` is running-in-a-tool and stays **running**. Ordinary permission prompts are recorded
in **no** transcript (the studio measured ~75,000 s of unattributable operator wait, backlog #72);
only the status file's `needs_input` can reveal one.

**2.1.9 Capacity.** Token: `~/.claude/.credentials.json` (a JSON document with `claudeAiOauth.
{accessToken, refreshToken, expiresAt, scopes, subscriptionType, rateLimitTier}`), tried first on
every platform; when absent **on macOS only**, the login Keychain item with service
`Claude Code-credentials`, read with exactly
`security find-generic-password -s "Claude Code-credentials" -w` (stdin closed, 5 s timeout, stderr
discarded unread because it names the keychain file), whose stdout is that same JSON document byte
for byte. Exit 44 = item absent = "no Claude Code credentials on this machine"; any other non-zero =
"the Keychain refused the read (security exit N)". Measured: the file is absent on this Mac; the
Keychain item is present.
Request: `GET https://api.anthropic.com/api/oauth/usage`, headers `Authorization: Bearer <token>`,
`anthropic-beta: oauth-2025-04-20`; timeout 5 s; the token lives in a `Secret` and is dropped when
the request is built. Response: JSON object with `five_hour` and `seven_day` objects, each
`{utilization: <number 0-100>, resets_at: <ISO-8601>, …}`, plus a dozen opaque keys (`used_dollars`
and null codenamed buckets) that are ignored. Handling per FR-12. The endpoint is **undocumented**;
a renamed key is drift → absence.
Provenance: `capture/usage_capacity.py:153-165, 226-238, 598-678, 838-993`.

**2.1.10 Resume.** `claude --resume <uuid>` with the PTY's cwd = the row's `cwd`. A blank or
whitespace id is not a resume (the CLI fails on `--resume ""`).

### 2.2 Codex CLI (0.149.1 on this machine)

**2.2.1 Rollout files.** `~/.codex/sessions/YYYY/MM/DD/rollout-<ISO-ts>-<uuid>.jsonl` and the same
 under `~/.codex/archived_sessions/`. Root override: `CODEX_HOME`. Measured: 72 rollout files,
 68 top-level logical sessions and 4 subagent-like files. A **resume
writes a new file for the same thread**, so a thread is a group of files.

**2.2.2 Record envelope.** `{"timestamp": <ISO-8601 Z>, "type": <string>, "payload": {…}}`. Types
seen: `session_meta` (1 per file, first line), `event_msg`, `response_item`, `turn_context`,
`compacted`, `world_state`. Lines may reach 1.26 MB.

**2.2.3 Identity of a thread (the first ≤3 lines).** From `session_meta.payload`: own id =
`id`, falling back to `session_id`; parent = `parent_thread_id`, falling back to a `session_id`
that differs from `id`. Also `cwd`, `timestamp`, `cli_version`, `model_provider`, `thread_source`,
`git {commit_hash, branch, repository_url}`, `originator`, `source`. **The sid is the whole UUID**;
UUIDv7's first 8 hex characters change only every ~65 s and collided in 8 of 110 studio rollouts.
**Exclude** a file when `session_id` is present and differs from `id`, or `thread_source ==
"subagent"` (4 of 71 here) — it is a subagent's rollout, not a resumable session.
Grouping: files with equal own id form one row; head fields from the file with the earliest
`session_meta.timestamp`; `last_active_ms` = max mtime across the group; `files` = the count.
Provenance: `capture/discovery.py:122-243`, ADR-0046.

**2.2.4 Title.** In order: (1) `thread_name` from `~/.codex/session_index.jsonl` (lines
`{"id","thread_name","updated_at"}`; only named threads appear — 12 of 71 here) keyed by own id;
(2) the first `event_msg` whose `payload.type == "user_message"` → `payload.message` (a string;
the payload also carries `client_id`, `images`, `local_images`, `audio`, `local_audio`,
`text_elements`); (3) the first `response_item` with `payload.type == "message"` and `payload.role
== "user"` whose joined `content[].text` does **not** start with `<environment_context>`,
 `<user_instructions>`, `<recommended_plugins>` or `# AGENTS.md` (in the current sampled files the naive first user
item is one of these wrappers); (4) `(untitled)`. Rule (2) is absent in some `history_mode:
"paginated"` files, so (3) is load-bearing. Head budget 64 KB, extended once to 1 MB when no
candidate was found.

**2.2.5 Counters.** Full scan of every file in the group. For each `event_msg` with `payload.type
== "token_count"` and an object `payload.info.last_token_usage`:
`{input_tokens, cached_input_tokens, cache_write_input_tokens, output_tokens,
reasoning_output_tokens, total_tokens}`. Mapping to the raw six (Anthropic semantics: full prompt =
input + cache read + cache write): `cache_read += cached_input_tokens`; `cache_write +=
cache_write_input_tokens`; `input += input_tokens − cached_input_tokens` (the wire's `input_tokens`
**includes** the cached portion; `cached > input` is drift → the file's counters are
`Unavailable`, never clamped); `output += output_tokens`; `reasoning += reasoning_output_tokens`
(shown separately, not one of the six). **API calls** = number of such events. **Tool calls** =
`response_item` with `payload.type ∈ {function_call, custom_tool_call, local_shell_call}`
(observed: 251 `custom_tool_call` in one file). **User turns** = `user_message` events, or when a
file has none, user `response_item` messages that pass the wrapper filter of §2.2.4(3).
**Duration** = last − first `timestamp` across the group. Cross-check T-16.3: the last
`payload.info.total_token_usage` of a file equals that file's summed deltas within rounding.
Provenance: `adapters/codex/parser.py:705-724` (`_map_usage`, ADR-0016 decision 2).

**2.2.6 Identity.** `~/.codex/auth.json` → `auth_mode` (e.g. `chatgpt`), `tokens.account_id`
(shown as its first 8 characters), `last_refresh`. `tokens.access_token`, `tokens.id_token`,
`tokens.refresh_token`, `OPENAI_API_KEY` are **never read into memory as values**: the parser
deserialises into a struct that has no fields for them. Plan word = `rate_limits.plan_type` of the
capacity reading (§2.2.7) when present.

**2.2.7 Capacity.** Newest ≤10 rollouts by mtime (`sessions/` and `archived_sessions/`). For each,
a tail read: seek to `len − 64 KB`, discard the partial first line, scan the complete lines for the
**last** one containing both `"token_count"` and `"rate_limits"`; on miss grow to 512 KB, then 4 MB,
then give up on that file. Parse `payload.rate_limits` (fallback `payload.info.rate_limits`):
`{limit_id, limit_name, plan_type, primary, secondary, credits, individual_limit,
spend_control_reached, rate_limit_reached_type}`; each slot `{used_percent: f64, window_minutes:
i64, resets_at: i64 epoch seconds}` or `null`. **Identify the window by `window_minutes`** (300 →
five_hour, 10080 → weekly), never by slot name: a live 2026-08-08 studio sample carried the weekly
window in `primary` with `secondary: null`. Unknown minutes → skipped; a non-object slot or a
non-numeric `used_percent` → drift. `stale = mtime of the file that answered older than 3600 s`
(flag on an `ok` reading). Both windows null with `rate_limit_reached_type` non-null → reached-limit
(FR-13). Measured: last `token_count` sits 1.2 KB – 408 KB from EOF in the 10 newest files.
Provenance: `capture/usage_capacity.py:291-301, 1873-1999`, ADR-0043.

**2.2.8 Live status.** A live `codex` process holds its rollout open (seen with `lsof -p`: the
rollout at fd 41, `~/.codex/thread-writer-locks/<thread-uuid>.lock` at fd 42). Mapping pid → thread:
(1) argv `codex resume <uuid>` when present; else (2) `lsof -p <pid> -Fn` filtered to
`rollout-*.jsonl` (macOS/Linux; the Windows arm is a second-pass item). Turn state from the
rollout's tail (same tail reader as §2.2.7): the last `event_msg` among `task_started` (opens a
turn), `task_complete` / `turn_aborted` / `error` (close it), and `user_message` (a user message
after a closing event means a new turn is starting → running) — the studio's seam,
`adapters/codex/parser.py:98-107, 1230-1280`. Types seen in 10 files: `task_started` 114,
`item_completed` 2115, `token_count` 901, `task_complete` 94, `thread_settings_applied` 110,
`turn_aborted` 20. Time-in-state advances on the newest **work** record after the opening event
(`response_item`, `token_count`, turn events), never on `session_meta`, `turn_context` or
`thread_settings_applied` — the studio measured 5.6 % of real turns aged out when the clock was
frozen at `task_started`, and a resume's scaffolding re-dating a dead turn when it was not.
**No approval-request event was present in any sampled rollout** — and the studio measured the same
over 142 rollouts on a machine with `approval_policy = on-request` (zero `request_user_input`
records) — so Codex cannot show **needs you** from files in the first pass (OQ-3). Processes named `codex` with argv containing
`app-server` are the IDE extension's servers and are not sessions.

**2.2.9 Resume.** `codex resume <uuid>` in the row's `cwd` (`codex resume --help`: `[SESSION_ID]`
is "UUID or session name; UUIDs take precedence").

### 2.3 OpenCode (1.18.29 on this machine)

**2.3.1 Database.** `~/.local/share/opencode/opencode.db`, SQLite in WAL mode, written live by the
engine (38 MB here, `-wal` and `-shm` present). Opened per FR-4 with `mode=ro`. Tables used:

```sql
session(id TEXT PK, project_id, parent_id, slug, directory, title, version, share_url,
        summary_additions, summary_deletions, summary_files, summary_diffs, revert, permission,
        time_created INTEGER /* epoch ms */, time_updated INTEGER /* epoch ms */, time_compacting,
        time_archived, workspace_id, path, agent, model, cost REAL,
        tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write, metadata)
message(id, session_id, time_created, time_updated, data TEXT /* JSON */)
part(id, message_id, session_id, time_created, time_updated, data TEXT /* JSON */)
project(id, worktree, vcs, name, …)
permission(id, project_id, action, resource, time_created, time_updated)
account(id, email, url, access_token, refresh_token, token_expiry, …)   -- email column ONLY
```
Other tables (`credential`, `control_account`, `session_share`, `todo`, `event`, `workspace`, …)
are never read.

**2.3.2 Rows.** `SELECT id, directory, title, time_created, time_updated FROM session WHERE
parent_id IS NULL AND time_archived IS NULL ORDER BY time_updated DESC LIMIT 2000`. `title` is the
title; `directory` is the cwd; `time_updated` (ms) is last-active. Measured: 28 rows, 5 of them
children. `slug` (random words) is not shown. `model` is not read (UR-17).

**2.3.3 Counters.** From the same row: `tokens_input`, `tokens_output`, `tokens_cache_read`,
`tokens_cache_write`, `tokens_reasoning` (separate), `cost` (the engine's own USD figure, shown
labeled). **API calls** = `SELECT COUNT(*) FROM part WHERE session_id=? AND
json_extract(data,'$.type')='step-finish'` (each step-finish part carries `tokens {input, output,
reasoning, cache{read, write}}` and `cost`; one step = one model call). **Tool calls** = parts of
type `tool` (`data {type, tool, callID, state{status, input, output, metadata, time, title}}`).
**User turns** = `SELECT COUNT(*) FROM message WHERE session_id=? AND
json_extract(data,'$.role')='user'`. **Duration** = `time_updated − time_created`. Cross-check
T-16.4: Σ step-finish `tokens` equals the session columns. Current Mac audit 2026-09-13: all 24
top-level, unarchived sessions have non-null session token columns; 23 have one or more
`step-finish` parts, and 21 have non-zero provider-reported cost. The remaining top-level row is a
valid zero-call session. Every sampled `step-finish` part carried input, output, reasoning,
cache-read, cache-write, and cost fields; the sums matched the session columns for all 23 sessions
with step data.

**2.3.4 Identity.** `~/.local/share/opencode/auth.json`: an object keyed by provider name, each
`{type, key}` (here `opencode` and `opencode-go`). Pigeon reports provider names and `type` only;
`key` is never deserialised. `SELECT email FROM account LIMIT 1` when the table has rows (0 here).

**2.3.5 Live status.** Processes named `opencode`; cwd from the process table. Sessions sharing a
directory are matched to the most recently active candidates up to the number of live processes,
so one process cannot make every historical session appear live. When more than one process or
candidate is involved, the session remains observable but its pid is null and stopping is refused as
ambiguous. Turn state: the session's newest `message` with `role == "assistant"`: `data.time.
completed` null → running; present (`finish` e.g. `stop`) → finished. `permission` rows whose
`project_id` equals the session's project → **needs you** (0 rows at measurement; semantics to be
confirmed at first occurrence, OQ-4).

**2.3.6 Capacity.** None: OpenCode is provider-agnostic and states no allowance. The card says
"capacity not exposed by this engine". `opencode` supports `--session <id>`; `--fork` exists and is
not used.

**2.3.7 Resume.** `opencode --session <id>` in the row's `directory`.

### 2.4 Provider metric coverage audit (owner Mac, 2026-09-13)

This audit is the acceptance boundary for the six counters and three API-owned KPIs. It reads the
current provider sources read-only; it does not change the source files or credentials.

| Provider | Eligible sessions | Sessions with model-call evidence | Raw counter result | KPI result |
|---|---:|---:|---|---|
| Claude Code | 50 | 48 | All four usage fields are present and non-zero in all 48 usage-bearing transcripts; two files are user/setup-only with no assistant call | All three formulas are computable when `api_calls > 0`; zero-call rows return `NULL` KPIs |
| Codex CLI | 68 top-level logical sessions | 56 | Input, cached input, output, reasoning, and total usage are present for all 56; 12 rows contain no model call/tool evidence and are valid zero-call sessions | Context and batching are computable for call-bearing rows; cache-write is missing/zero in the current corpus, so rewrite ratio has no observed positive signal and must be labelled accordingly |
| OpenCode | 24 top-level, unarchived sessions | 23 | All 24 session rows have token columns; all 1,131 sampled `step-finish` parts carry input/output/reasoning/cache-read/cache-write/cost; one row is a valid zero-call session | All three formulas are computable when calls exist; cost is provider-reported and available on 21/24 as non-zero |

**Decision.** Pigeon may ship the raw six and API-owned KPI formulas for all three providers. It
must distinguish valid zero-call/undefined rows from unavailable data, and it must label Codex's
rewrite ratio as a current zero/no-positive-signal measurement rather than implying that Codex emits
the same cache-write behavior as Claude or OpenCode.

---

## 3. Functional requirements

Format: statement · behaviour · errors · acceptance tests · traces · origin · provenance.

### FR-1 Engine registry and adapter contract
**Statement.** The host MUST hold one registry of engines, each implementing one trait, and every
other layer MUST be engine-agnostic.
**Behaviour.**
1. `ProviderId` is the closed set `claude-code | codex | opencode` (serde kebab-case), and is the
   only engine word the view knows.
2. `trait Engine { id(); program() -> &str; resume_argv(sid) -> Vec<String>; sessions() ->
   EngineReport; identity() -> Result<Identity, EngineError> }`, plus optional
   `CapacitySource::capacity()`, `MetricsSource::metrics(&SessionRow)`, `StatusSource::observe()`.
   An engine that does not implement `CapacitySource` reports `supported: false`, which is not an
   error.
3. `EngineReport { rows, problem: Option<EngineError>, unknown_types: BTreeMap<String,u32> }`: one
   engine's missing root or unreadable file never blanks another engine's rows.
4. Each adapter is constructed with its root(s) (`ClaudeEngine::new(root)`) so tests point it at
   fixtures; production roots come from the home directory and the engine's own override variable
   (`CODEX_HOME`).
5. `program()` names the binary bare (`claude`, `codex`, `opencode`); FR-8 resolves it on PATH.
**Tests.** T-1.1 compiling out one adapter changes no test result for the other two. T-1.2
`resume_argv` for the three engines equals §2.1.10, §2.2.9, §2.3.7 exactly. T-1.3 a registry with
a fixture root that does not exist yields `rows=[]` and `problem.kind=root_missing`.
**Traces.** UR-3, UR-15. **Origin.** owner (studio invariant 8). **Provenance.** ADR-0001, ADR-0016.

### FR-2 Claude Code session discovery
**Statement.** The Claude adapter MUST produce one row per main transcript per §2.1.1–§2.1.5.
**Behaviour.** Enumerate directories; take depth-1 UUID-stem `.jsonl` files; head-read each per
§2.1.3; `sid` = stem; `cwd` from the record; `project` = normalised cwd (FR-7); `title` per
§2.1.5; `first_active_ms` from `first_ts`; `last_active_ms` = mtime; `resumable` = cwd exists and
`claude` resolves (FR-8). Files with no `cwd` in the head get `cwd: null` and are not resumable
(reason "transcript names no working directory").
**Errors.** Root missing → `root_missing`; a file that cannot be opened → counted in
`problems[]` with its path, row omitted; unknown record types tallied.
**Tests.** T-2.1 fixture with string content and list content titles. T-2.2 a `tool_result` first
user record is skipped for the title. T-2.3 `isCompactSummary` and `isMeta` records skipped. T-2.4
subdirectories `memory/` and `<uuid>/` and a `notes.jsonl` are ignored and the stray file is
tallied. T-2.5 `cwd` is taken from an `attachment` record preceding the first user record. T-2.6 a
truncated last line does not fail the file. T-2.7 title upgrades to the last `ai-title` after the
fold. T-2.8 a newer `subagents/agent-x.jsonl` moves `lastActiveMs` past the main file's mtime.
**Traces.** UR-3. **Origin.** joint. **Provenance.** `capture/discovery.py:33-47`,
`capture/session_meta.py:10-21, 712-734`.

### FR-3 Codex session discovery
**Statement.** The Codex adapter MUST produce one row per thread per §2.2.1–§2.2.4.
**Behaviour.** Glob both roots recursively; read ≤3 lines per file for `session_meta`; exclude
subagent files; group by own id; representative = earliest; title chain; `session_index` overlay
keyed by id; `last_active_ms` = max mtime; `files` = group size; `resumable` = cwd exists and
`codex` resolves.
**Tests.** T-3.1 two files with one id → one row, `files=2`, head fields from the earlier file,
last-active from the later. T-3.2 a `thread_source: "subagent"` file and a `session_id != id` file
are excluded. T-3.3 title chain: index name wins; else `user_message`; else the first user item
not starting with the four wrappers; a file whose only user items are wrappers is `(untitled)`.
T-3.4 the sid is the full UUID (two ids sharing an 8-char prefix are two rows). T-3.5 a 100 KB
single-line record inside the head budget does not break parsing.
**Traces.** UR-3. **Origin.** joint. **Provenance.** `capture/discovery.py:122-243`, ADR-0046.

### FR-4 OpenCode session discovery
**Statement.** The OpenCode adapter MUST read the live database read-only per §2.3.1–§2.3.2.
**Behaviour.**
1. Open with `rusqlite::Connection::open_with_flags("file:<abs path>?mode=ro",
   SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX | SQLITE_OPEN_URI)`; then `PRAGMA query_only=1`;
   `busy_timeout(250 ms)`; one retry on `SQLITE_BUSY`. **Never `immutable=1`** (it bypasses the WAL
   and can read a torn, pre-checkpoint image).
2. `mode=ro` cannot create the file; a missing database is `root_missing`; `SQLITE_CANTOPEN` is an
   absence, never a write.
3. The query of §2.3.2; `time_updated` is epoch **milliseconds**; `directory` normalised for
   `project`.
4. `rusqlite` with the `bundled` feature so the SQLite version is pigeon's, not the OS's.
**Tests.** T-4.1 a WAL database built in a tempdir with parent, archived and top-level rows yields
only top-level, unarchived rows. T-4.2 a nonexistent path → `root_missing` and the file still does
not exist afterwards. T-4.3 the database's mtime is unchanged after ten reads. T-4.4 a database held
open by a writer in a second connection mid-transaction is still readable.
**Traces.** UR-3, UR-11, UR-15. **Origin.** agent.

### FR-5 Scoped session list
**Statement.** `sessions_list` MUST return provider rows merged into the requested `live` or
`recent` scope, with problems.
**Behaviour.** Concatenate `EngineReport.rows`; for `live`, retain only rows attached to a live
process observation; for `recent`, retain only rows whose `last_active_ms >= now - 7 days`. Sort by
`last_active_ms` desc, then `engine`, then `sid`; attach metrics from the cache (FR-18); return
`{scope, since_ms, rows, problems, generated_at_ms}`. Cached 10 s (FR-25); `force: true` bypasses.
A refresh emits `sessions://changed` when the scoped row set or any `last_active_ms` changes.
**Tests.** T-5.1 fixture roots for all three engines → one scoped sorted list; T-5.2 one engine's root
missing → the other two engines' rows plus one problem; T-5.3 identical `last_active_ms` orders by
engine then sid; T-5.4 a session older than seven days is absent from `recent`.
**Traces.** UR-1, UR-3. **Origin.** owner.

### FR-6 Session detail
**Statement.** Selecting a row MUST show, in the right pane: engine, title, engine-given name (if
any), full cwd, git branch (Claude only; Codex `git.branch` when present), first active and last
active (local), duration, live status with time-in-state and evidence, the raw six spelled out,
reasoning tokens where the engine reports them, the three KPIs each with its one-line meaning, user
turns, OpenCode's own cost labeled "engine-reported cost", the source path(s), file count (Codex),
and any `unknown_types`. It MUST NOT show the model (UR-17), the transcript, or any chart.
**Tests.** T-6.1 a Claude row renders branch and no model; T-6.2 an OpenCode row renders cost with
the label; T-6.3 pending metrics render a "counting…" state, not zeros.
**Traces.** UR-1, UR-5, UR-17. **Origin.** owner.

### FR-7 Project-first scoped rollup
**Statement.** `projects_summary(scope)` MUST return only projects represented by the selected
session scope and roll their counters up per §7.4 rules.
**Behaviour.** Normalisation: absolute path, trailing separators stripped, `.`/`..` segments
collapsed, case-folded on Windows only; `project_leaf` = last segment. Per project: `sessions`,
  `status_counts`, `providers: {provider: count}`, `counted` (rows with metrics), the raw six summed over counted rows,
  `user_turns` and `duration_ms` summed, `provider_cost_usd` summed over rows that report one (with
`cost_rows`), KPIs recomputed from the sums (FR-17), `last_active_ms` = max. Rows with `cwd: null`
  form a project labeled `(no directory)` only when they are in the selected scope. A project with no
  qualifying session is omitted.
**Tests.** T-7.1 sums equal the sum of rows; T-7.2 KPIs equal formulas over sums, not mean of row
KPIs; T-7.3 `counted < sessions` while one row's metrics are pending; T-7.4 `/a/b/` and `/a/b`
merge; T-7.5 on Windows `C:\A` and `c:\a` merge (unit test with the Windows fold function).
**Traces.** UR-3, UR-6. **Origin.** owner.

### FR-8 Console: resume
**Statement.** `console_open({sessionKey, cwd, cols, rows})` MUST spawn the engine's resume
command in a PTY in `cwd` and return `{id}`.
**Behaviour.**
1. Resolve `sessionKey.engine.program()` on the effective PATH (FR-28) at spawn time; not found → error
   `not_installed` with the program name, no spawn.
2. `cwd` must be an existing directory and must match the host-resolved cwd for `sessionKey`; else
   `path` or `invalid_argument` error. The View cannot redirect a session into an arbitrary folder.
3. argv = `[program] + sessionKey.engine.resume_argv(sessionKey.sid)`; on Windows a `.cmd`/`.bat` shim is wrapped in
   `%ComSpec% /c` (studio's `launch_argv`); no argument ever contains a space (checked by T-8.4).
4. PTY size = clamp(cols, rows) to 1..5000; child env = FR-28's env.
5. Registry entry `{id, session_key, cwd, started_at_ms, running: true, exit_code: None}`;
   `id` = a random 16-hex string.
6. The output pump thread starts parked on the ready gate (FR-9) and the supervisor thread waits
   on the child.
7. **Locking:** the registry mutex is held for one map insertion; never across resolve, spawn,
   write, kill, wait or emit.
**Tests.** T-8.1 `resolve_on_path` finds a program in a temp PATH dir and not elsewhere; T-8.2 a
missing cwd is refused before spawn; T-8.3 the three engines' argv; T-8.4 no argv element
contains whitespace for any UUID/`ses_` id; T-8.5 (manual, macOS) banner visible ≤ 2 s.
**Traces.** UR-4. **Origin.** owner. **Provenance.** `app/src-tauri/src/console.rs:449-638,
860-1053` (stripped of `env`, `profile`, `profileId`, `command`, `refuse_stock_home`).

### FR-9 Console: I/O, resize, ready, close, list, exit
**Statement.** The five remaining console commands and two events MUST behave as the studio's
frozen seam, minus the dropped fields.
**Behaviour.**
- `console_input({id, dataB64})` writes decoded bytes to the PTY master; unknown id → error.
- `console_resize({id, cols, rows})` clamps and resizes.
- `console_ready({id})` opens the pump gate; idempotent. The gate exists because a Windows
  ConPTY emits a cursor-position query and suspends the child until answered by a live terminal; on
  unix it is harmless and kept so the view's attach order is one order everywhere.
- `console_close({id})` kills the child (TERM then KILL after 2 s on unix; `TerminateProcess` on
  Windows) and removes the entry after exit.
- `console_list()` → `{consoles: [{id, sessionKey?, engine, cwd, mode, running, exitCode, startedAtMs}]}`.
- Event `console://data {id, dataB64}` chunks ≤ 8 KiB; event `console://exit {id, exitCode}` once.
- All payloads base64 both ways (a TUI byte stream is not valid UTF-16).
- On window destroy the host kills every console.
**Tests.** T-9.1 the ported studio tests for `pump`, `list_rows`, `gate_to_open`, chunking; T-9.2 a
console closed before ready does not leave a parked thread; T-9.3 exit emits exactly one event.
**Traces.** UR-4. **Origin.** owner. **Provenance.** `console.rs:157-244, 640-734, 1071-1241`.

### FR-10 Console and the running badge
**Statement.** The view MUST render the selected session's terminal in the existing right pane,
below that session's metrics, and reflect the hosted state on the session row.
**Behaviour.** Resume opens an in-place terminal context titled `<engine> · <project leaf>`; an
existing console for the same `SessionKey` is focused instead of a second spawn (UR-4 A6). A new
session starts an in-place terminal with no `SessionKey` until discovery observes the new engine
session. The selected identity and metrics remain visible above the terminal. The terminal shows the
exit notice and code and stays until closed. Multiple consoles may be tracked by the host, but only
one is visible in the right pane at a time; opening one does not require a browser tab, Tauri window,
or separate page. The row for a hosted console gets its status from the console (FR-22 rule 1).
The host keeps a bounded scrollback buffer for every Pigeon-owned console. Switching sessions
detaches the View without killing the console; selecting it again replays the buffer before live
output. Closing the application terminates every Pigeon-owned console process. Attach order in the
terminal component is exactly:
subscribe data/exit → create terminal (after `document.fonts.ready`) → fit → `console_resize` →
`console_list` → `console_ready` last.
**Tests.** T-10.1 vitest: `ready` is called last and after `list`; T-10.2 a second Resume on the
same row calls no `console_open`; T-10.3 `windowsPty` option is set only when `host_info().os ==
"windows"`.
**Traces.** UR-1, UR-4. **Origin.** owner. **Provenance.** `app/src/console/ConsoleView.tsx:
123-277`, `terminalEngine.ts:88`, BACKLOG #114.

### FR-11 Login identity per engine
**Statement.** `account_status` MUST report each engine's identity per §2.1.7, §2.2.6, §2.3.4.
**Behaviour.** Cached 5 min. Claude: the credential blob is read for two plan words only; the
parser's struct has no token fields. Codex: struct without token fields. OpenCode: provider names
and types; `email` from `account`. Missing file → `signed_in: false` with a reason.
**Tests.** T-11.1 a fixture `~/.claude.json` → label/org/tier; T-11.2 a credential blob fixture
with sentinel tokens → plan words present, sentinels absent from the serialised result and `{:?}`;
T-11.3 Codex auth fixture → mode and truncated account id, sentinels absent; T-11.4 OpenCode auth
fixture → names and types, `key` absent.
**Traces.** UR-2, UR-12. **Origin.** owner.

### FR-12 Claude Code capacity
**Statement.** The Claude capacity reading MUST follow §2.1.9 and produce either two windows or a
stated absence.
**Behaviour.**
1. Load token: file, then Keychain (macOS). Absent → `no_credential` "no Claude Code credentials on
   this machine". Unparseable → `unknown_shape` "could not read the Claude credentials file".
2. GET with a 5 s timeout via one shared async client. Transport failure → `transport` with the
   error **class** only ("could not reach the usage endpoint (Timeout)").
3. 401/403 → `credential_refused` "the usage endpoint refused the stored login (HTTP 401) — it
   likely expired; start a new `claude` session to refresh it" (append " — and the stored login's
   own expiry stamp is already past" when `expiresAt` < now). 429 → `http_status` with
   `Retry-After` honoured (next poll no sooner than that, minimum 60 s). Other → `http_status`.
4. Body not JSON / not an object → `unknown_shape`. For each of `five_hour`, `seven_day`: `null` →
   that window **absent**; object → `utilization` must be a number 0..100 and `resets_at` a string
   or null, else drift. Both absent → `unknown_shape` "the usage endpoint reported no 5-hour or
   weekly window". Any drift → "the usage endpoint's shape has changed: <field list>".
5. Result `{supported: true, windows: [...], read_at_ms, problem}`; cached 15 min; `force`
   refreshes.
**Tests.** T-12.1 the studio's redacted `claude_oauth_usage_200.json` → two windows with the
percentages it states; T-12.2 `{"five_hour": null, "seven_day": {...}}` → one window; T-12.3
missing both → absence; T-12.4 401 → `credential_refused`; T-12.5 the leak table (FR-29) across
every branch; T-12.6 Keychain exit 44 → `no_credential`, exit 1 → "refused (security exit 1)" and
stderr text absent from everything.
**Traces.** UR-2, UR-13. **Origin.** owner. **Provenance.** `capture/usage_capacity.py:838-993`.

### FR-13 Codex capacity
**Statement.** The Codex capacity reading MUST follow §2.2.7.
**Behaviour.** No file → `root_missing` "no Codex sessions on this machine"; no file answers →
`unknown_shape` "no recent Codex session states a rate limit"; drift → "the Codex rate-limit
record's shape has changed: …"; reached-limit → `unsupported`-class absence with the engine's word:
"Codex reports a reached limit: <rate_limit_reached_type>" and **no bars** (never draw the previous
green windows over a blocked account). `stale` is a flag on an `ok` reading, shown as "reading from
<time>, stale". `plan_type` feeds identity. **Pigeon never runs `codex` to refresh a reading**: a
read that spends a turn would measure what it changes.
**Tests.** T-13.1 the studio's `codex_token_count_*.jsonl` fixtures → windows identified by
minutes; T-13.2 weekly-in-`primary` sample → weekly; T-13.3 a slot that is a string → drift; T-13.4
a 100 KB filler line before the record → found by the doubling tail; T-13.5 mtime older than 3600
s → `stale: true`; T-13.6 reached-limit sample → absence with the word.
**Traces.** UR-2, UR-13. **Origin.** owner. **Provenance.** `capture/usage_capacity.py:1873-1999`.

### FR-14 OpenCode capacity
**Statement.** OpenCode reports `supported: false`; the card shows "capacity not exposed by this
engine" and never a bar.
**Tests.** T-14.1 the OpenCode card renders the sentence and no bar element.
**Traces.** UR-2, UR-13. **Origin.** agent.

### FR-15 Account strip
**Statement.** The top strip MUST show one card per engine with identity, capacity and status of
the reading.
**Behaviour.** Card = engine badge · label (e-mail/name) · organisation/plan words · two bars
(5-hour, weekly) each with `used %` and "resets HH:MM" local · "read HH:MM" · stale flag ·
problem sentence when any · a refresh button per card. Not installed → a quiet chip. Bars: the
studio's severity colours by used percentage (<50 calm, <80 warn, ≥80 hot). No model name, no
dollars except OpenCode's cost which is not on the strip.
**Tests.** T-15.1 vitest renders three fixtures (ok, absent window, problem) correctly; T-15.2 the
Codex stale flag renders.
**Traces.** UR-1, UR-2, UR-17. **Origin.** owner.

### FR-16 Raw counters per engine
**Statement.** Counters MUST be computed per §2.1.6, §2.2.5, §2.3.3 and be identical to the
studio's for Claude Code.
**Tests.** T-16.1 three Claude sessions present in both products: six equal counters (manual,
recorded in `docs/verification/`); T-16.2 a streamed message across three records folds to one
call with the MAX per key and its `tool_use` id counted once; a subagent file restating the id adds
nothing; T-16.3 Codex: Σ deltas equals the file's last `total_token_usage`; `cached > input` →
`Unavailable`; T-16.4 OpenCode: Σ step-finish tokens equals the session columns for the fixture DB.
**Traces.** UR-5, UR-13. **Origin.** owner (invariant 3). **Provenance.** as cited per engine.

### FR-17 KPI formulas
**Statement.** The three KPIs MUST be exactly:
- `context_per_call = cache_read / api_calls` (tokens per call);
- `rewrite_ratio = cache_write / cache_read`;
- `batching_ratio = tool_calls / api_calls`;
each `None` when its denominator is 0, and for a project computed from the summed counters.
**Tests.** T-17.1 zero denominators → `None`; T-17.2 the studio's `formulas.py` doctest values
reproduce.
**Traces.** UR-5, UR-6, UR-13. **Origin.** joint. **Provenance.** `derive/formulas.py:42-47,
94-102`.

### FR-18 Lazy metrics fill and cache
**Statement.** Counters MUST be computed off the list path and cached by file signature.
**Behaviour.**
1. `sessions_list` never folds. Rows carry `metrics: {state: "pending"}` until filled.
2. A background task walks the current rows newest-first and computes metrics for each row whose
   cache key `(engine, sid, signature)` is absent, where signature = Claude: `(path, size, mtime)`
   of the main file plus the sidecar dir mtime; Codex: the sorted list of `(path, size, mtime)` of
   the group; OpenCode: `time_updated`. One worker thread, lowest priority, yields between files.
3. After each batch of ≤20 filled rows it emits `sessions://metrics {rows: [{key,
   metrics}], generatedAtMs}`. A failed fold emits `{state: "unavailable", error}` rather than
   silently returning zeroes.
4. `session_metrics({key})` computes on demand for a selected row and returns its `MetricState`.
5. The cache is memory-only, unbounded within reason (≤10k entries, LRU beyond).
**Tests.** T-18.1 a row's metrics are recomputed only when its signature changes; T-18.2 the list
returns before any fold runs (timing assert with a slow fixture).
**Traces.** UR-5, UR-6. **Origin.** agent.

### FR-19 Live status: Claude Code
**Statement.** The Claude status source MUST read `~/.claude/sessions/*.json` per §2.1.8 every
`pollIntervalSeconds`.
**Behaviour.** For each `<pid>.json`: parse; check pid liveness; live → `Observed {engine, sid,
pid, cwd, name, raw_word: status, since_ms: statusUpdatedAt, evidence: ["sessions/<pid>.json"]}`;
dead → nothing (the row's `status` is `null` by absence: no badge, not counted). `.key` files are never opened. A `status`
word outside the mapping is passed through as `raw_word` with `state: unknown`.
**Tests.** T-19.1 fixture dir with a live pid (the test's own pid) and a dead pid → one observation;
T-19.2 `needs_input` → needs you, `busy` → running, `idle` → finished, `weird` → unknown with the
word; T-19.3 (manual) the observations equal `claude agents --json` on this Mac. T-19.4 an `idle` file
plus a tail whose terminal assistant message has an unanswered `AskUserQuestion` → needs you; the
same with an unanswered `Bash` → finished.
**Traces.** UR-7. **Origin.** joint.

### FR-20 Live status: Codex
**Statement.** The Codex status source MUST map live `codex` processes to threads per §2.2.8.
**Behaviour.** Every 3 s from the process table: processes whose executable name is `codex` (or
whose argv[0] ends in `/codex`) and whose argv does not contain `app-server`. For each: sid from
  argv `resume <uuid>` if present; else (every `pollIntervalSeconds`, cached by pid) `lsof -p <pid> -Fn`, take the
`rollout-*.jsonl` path's uuid. Turn state from the tail (§2.2.8): `task_started` (or a `user_message` after a closing event) →
running; `task_complete` / `turn_aborted` / `error` → finished; no such event → unknown ("no turn
event in the tail"). `since_ms` = the newest work record's timestamp while running, the closing
event's when finished. A running Codex row quiet for more than 20 minutes keeps its state (the
process is alive, which the studio never knew) but shows "quiet 24m" beside it. Windows: argv only; without a sid the process is
reported as an unattached live engine (shown in the rail as "codex running, session unknown").
**Tests.** T-20.1 argv parsing; T-20.2 `lsof -Fn` output fixture → path → uuid; T-20.3 tail state
for the three event types.
**Traces.** UR-7. **Origin.** agent.

### FR-21 Live status: OpenCode
**Statement.** The OpenCode status source MUST attach live `opencode` processes to sessions per
§2.3.5 every `pollIntervalSeconds`.
**Behaviour.** Process cwd (normalised) ↔ `directory`; newest `time_updated` wins; the newest
assistant message's `time.completed` decides running vs finished; a `permission` row for the
project → needs you (the newest session in that directory carries it). `since_ms` = the message's
`time.completed` or `time.created`.
**Tests.** T-21.1 fixture DB + a fake process list → states; T-21.2 two sessions in one directory →
one attached, one with no status.
**Traces.** UR-7, UR-15. **Origin.** agent.

### FR-22 Live status model, decision table and snapshot
**Statement.** The host MUST combine the sources into one `StatusSnapshot` per §6 and emit it.
**Behaviour.** `status_snapshot() -> {generated_at_ms, counts: {running, needs_you, finished,
unknown}, live: LiveRow[]}` where `LiveRow = {key: SessionKey, project_leaf, title, name, state,
since_ms, evidence: string[], pid, console_id}`. Sessions with no live process are not in `live`
and are counted nowhere (owner ruling 2026-09-12); their row carries `status: null`. The poller runs every `pollIntervalSeconds`; hosted consoles update
synchronously on open/exit; a changed snapshot emits `status://changed`. Row attachment in
`sessions_list` is by `SessionKey`.
**Tests.** T-22.1 the decision table in §6, one test per row; T-22.2 a hosted console for a Claude
sid with a dead status file is **running**, not status-less; T-22.3 counts equal the states of
`live`; T-22.4 a row with no live evidence has `status: null` and appears in no count.
**Traces.** UR-7, UR-9. **Origin.** owner.

### FR-23 The hover window
**Statement.** A second Tauri window labeled `hover` MUST show the live snapshot always on top.
**Behaviour.**
1. Config: `label: "hover"`, `visible: false` at start (settings decide), `alwaysOnTop: true`,
   `decorations: false`, `transparent: true`, `shadow: false`, `resizable: false`, `skipTaskbar:
   true`, `focus: false`, `width: 320`, `height: 240`, `visibleOnAllWorkspaces: true`. macOS
   transparency requires `app.macOSPrivateApi: true` in `tauri.conf.json` and the `macos-private-
   api` Cargo feature (the studio needed the same for its plate).
2. Content: a header with three counters (**running**, **needs you** in the hot colour,
   **finished**), then ≤8 rows ordered needs-you, running, finished, each by recency; each row:
   engine badge · project leaf · name or title (one line) · state badge · time-in-state; footer:
   "+N more" and "updated HH:MM:SS". Sessions with no live process never appear.
3. **Docking, copied from the studio's plate** (`app/src/telemetry/dock.ts`): the hover lives in one
   of four corners `tl | tr | bl | br` (keys `1`–`4` while it is focused, and four dock buttons in
   its footer), default `tr`. `dock_to(corner)` reads `currentMonitor()`, computes the rect
   absolutely from the monitor **work area** (hard into the corner, 12 pt margin on each edge,
   scaled by `scaleFactor`), and resizes in place before it moves. The header is also a drag region
   (`data-tauri-drag-region`) for a free position; a drop within 24 pt of a corner snaps to it.
   Corner or free position is saved (debounced 500 ms) and restored on launch, clamped onto a
   visible monitor.
4. Clicking a row: `hover_select(SessionKey)` → the host shows and focuses `main` and emits
   `feather://select-session SessionKey` to it; clicking the header only shows `main`.
5. Toggle from the main window's title area (`hover_toggle`), state in settings. The hover never
   takes keyboard focus and is excluded from the app switcher.
6. Capabilities: `capabilities/default.json` lists windows `["main", "hover"]` with
   `core:default`; `capabilities/hover.json` grants `hover` exactly `core:window:allow-start-
   dragging`, `allow-set-position`, `allow-set-size`, `allow-current-monitor`, `allow-hide` (the
   studio's `telemetry-widget.json` set). No `set-ignore-cursor-events` grant to the view; ghosting
   is a host command.
7. **Freshness.** Repaints on `status://changed`; a 30 s tick re-renders time-in-state while the
   hover is visible and is skipped while `document.hidden`. On any read failure every figure blanks
   to `—` rather than going stale (the studio's `plateModel` rule), with the catalogue sentence in
   the footer.
8. **macOS gap, stated:** the studio clips its Windows plate to a per-pixel silhouette with
   `SetWindowRgn`; Tauri offers nothing equivalent on macOS, so the hover's full rectangle captures
   the mouse. Hence the compact 320 × 240 box with no unpainted area. A `ghost` mode (whole-window
   `set_ignore_cursor_events(true)` for 3 s from a host timer, restored by the host) is a MAY.
**Tests.** T-23.1 vitest: ordering and the 8-row cap; T-23.2 the header shows exactly three counters
and no count of status-less sessions; T-23.3 (manual) drag, quit, relaunch → same position and visibility; T-23.4
(manual) the hover stays above a full-screen Terminal window and never steals focus.
**Traces.** UR-8. **Origin.** owner.

### FR-24 In-app status surface
**Statement.** The main window MUST show status on every row, in a status rail, in a live filter
and on the detail pane.
**Behaviour.** `StatusBadge` on every row that has a status (text + colour; rows with `status:
null` show none); `StatusRail` above the list with the same three counters as the hover and a
**live** toggle that filters to rows with a status while keeping sort; the detail pane shows state, time-in-state, evidence lines (e.g.
`sessions/35742.json · status=busy · pid alive`) and, for hosted consoles, an in-place terminal
link/control.
**Tests.** T-24.1 filter keeps order and hides status-less rows; T-24.2 rail counts equal the snapshot.
**Traces.** UR-1, UR-9. **Origin.** owner.

### FR-25 Refresh and cache model
**Statement.** Every reading MUST come from a typed cache slot with a TTL and coalesced refresh.
**Behaviour.** Slots: sessions 10 s; identity 5 min; capacity 15 min (Claude also honours
Retry-After); status snapshot 3 s poller (not a TTL slot); metrics by signature. Refresh triggers:
TTL expiry on next read; `force` from the view's `window.focus` listener (sessions) and refresh
buttons (capacity, identity); `console://exit` (sessions + status). Concurrent refreshes of one slot
coalesce behind a per-slot async mutex held across the refresh; the std mutex guarding the value is
never held during I/O. Every slot value carries `read_at_ms`.
**Tests.** T-25.1 two concurrent forced refreshes run the loader once; T-25.2 TTL expiry reloads;
T-25.3 Retry-After 120 defers the next capacity load ≥120 s.
**Traces.** UR-2, UR-3, UR-10. **Origin.** agent.

### FR-26 Errors on screen and drift handling
**Statement.** Every failure MUST reach the screen as a fixed sentence from the catalogue (§9),
never as a formatted exception, and never as a number.
**Behaviour.** `EngineError {engine, kind, detail}` serialises to `{engine, kind, detail, message}`
where `message` is looked up from the catalogue by `(kind, detail)`. Unknown record types are
tallied, not fatal. A field with an unexpected type is drift for that reading only.
**Tests.** T-26.1 every `ErrorKind × Detail` combination has a catalogue sentence (exhaustive
match); T-26.2 serialisation carries no `{:?}` of any source error.
**Traces.** UR-2, UR-13. **Origin.** owner (invariant 2).

### FR-27 Platform gating and host info
**Statement.** `host_info() -> {os: "macos"|"windows"|"linux", arch, version}` MUST be the only
platform signal the view uses. Rust platform differences live in `console.rs` (`launch_argv`,
`executable_extensions`), `status.rs` (lsof vs argv-only), and `pathenv.rs`.
**Tests.** T-27.1 `cargo check --all-targets` on the three targets; T-27.2 vitest: `windowsPty`
only on windows.
**Traces.** UR-14. **Origin.** agent.

### FR-28 PATH and environment bootstrap for GUI launches
**Statement.** A pigeon launched from Finder/Dock MUST find the engines and give TUIs a sane
environment.
**Behaviour.** On macOS and Linux at startup: run `$SHELL -lc 'printf %s "$PATH"'` once (3 s
timeout; failure → proceed without), union with the process PATH and with `~/.local/bin`,
`~/.bun/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, `~/.cargo/bin`, `~/.npm-global/bin`,
preserving order and removing duplicates; use it for `resolve_on_path` and as the child's `PATH`.
Child env additions when unset: `TERM=xterm-256color`, `COLORTERM=truecolor`, `LANG=en_US.UTF-8`.
Windows: the process PATH plus `PATHEXT` handling (studio's rule). Measured: `launchctl getenv PATH`
is unset on this Mac, so a Finder launch inherits `/usr/bin:/bin:/usr/sbin:/sbin` and none of the
three engine binaries.
**Tests.** T-28.1 union/dedupe order; T-28.2 (manual) Resume works from a packaged `.app`.
**Traces.** UR-4, UR-14. **Origin.** agent.

### FR-29 Secret hygiene
**Statement.** No credential MUST ever appear in a log, an error, a serialised payload, the view or
a subprocess argument.
**Behaviour.** `Secret(String)` with `Debug`/`Display` printing `[redacted]`; parsers for
credential files deserialise into structs without token fields except the one `Secret`; every
`map_err` classifies (`e.status()` → `HttpStatus(u16)`) or drops the source; `EngineError.detail`
is a typed enum, never a free string; the Keychain read discards stderr; the log writer refuses a
line containing `Bearer ` or `sk-ant-` and logs a redaction marker instead.
**Tests.** T-29.1 the **leak table**: a credentials fixture with `accessToken:
"SENTINEL-TOKEN-7f3a…"` driven through every error branch of FR-11/FR-12 (401, 429, 500, garbage
JSON, timeout, Keychain exit 1) asserting `SENTINEL` is absent from `serde_json::to_string(&err)`,
`format!("{err:?}")`, the `account_status` JSON and the log file.
**Traces.** UR-12. **Origin.** owner (ADR-0043).

### FR-30 Threading and locking
**Statement.** No blocking work MUST run on the UI thread, and no lock MUST be held across work.
**Behaviour.** All list/status/metrics/identity/capacity commands are `async fn` and run
filesystem, SQLite and subprocess work inside `tauri::async_runtime::spawn_blocking` (a sync
command body runs inline in the IPC handler, which on macOS is the main thread). HTTP uses one
shared async `reqwest::Client` (`default-features=false, features=["rustls-tls","json"]`). The
console registry mutex guards one map mutation at a time. Pollers run on their own threads with
bounded work per tick.
**Tests.** T-30.1 a 2 s slow fixture load does not delay `console_input` round-trips (measured in
the dev build); T-30.2 the studio's registry-lock tests ported.
**Traces.** UR-11. **Origin.** owner (invariant 10).

### FR-31 Time handling
**Statement.** All timestamps in the host MUST be UTC epoch milliseconds; the view formats them
with `Intl.DateTimeFormat` in the local zone.
**Behaviour.** Relative when < 24 h ("2m ago", "3h ago"), "yesterday HH:MM", else "D MMM HH:MM";
durations as `<60s` → `42s`, `<1h` → `12m`, `<1d` → `3h 05m`, else `2d 4h`; reset times "HH:MM";
hover times "HH:MM:SS". No date crate in Rust: epoch ms only.
**Tests.** T-31.1 vitest formatting table.
**Traces.** UR-2, UR-3. **Origin.** owner (invariant 5).

### FR-32 Packaging, settings and first run
**Statement.** `npm install && npm run tauri dev` MUST run from a fresh clone on this Mac; `npm run
tauri build` MUST produce an unsigned `.app` and `.dmg`.
**Behaviour.** Bundle targets `["app", "dmg"]` on macOS; the README states the first pass is
unsigned and how to open it (right-click → Open). Settings file `<app-data>/config.toml`:
`{hover: {visible, x, y}, list: {view: "live"|"recent"}, pollIntervalSeconds: 5, recentWindowDays: 7, split: number}`; missing or corrupt →
defaults, never a crash. First run shows the empty right pane with a one-line hint "select a
project or session, or start one".
**Tests.** T-32.1 corrupt settings → defaults; T-32.2 (manual) fresh clone runs.
**Traces.** UR-10, UR-14. **Origin.** owner.

### FR-33 Logging and diagnostics
**Statement.** The host MUST write a JSON-lines log to `<app-data>/logs/feather.log` with rotation
at 5 MB × 3, and MUST expose the last 200 records in a small diagnostics panel behind the title
area.
**Behaviour.** Records `{ts, level, event, fields…}`; events for spawn (program path, engine,
cwd, no argv beyond the sid), console exit, poller errors, capacity classes (never bodies),
drift tallies. Redaction guard per FR-29.
**Tests.** T-33.1 rotation; T-33.2 the guard.
**Traces.** UR-12. **Origin.** agent.

### FR-34 Start a new session and choose a project
**Statement.** The host MUST let the owner choose a folder and start a new session in that folder
with Claude, Codex, or OpenCode.
**Behaviour.** `project_pick` opens the native folder picker and returns a normalized path or null on
cancel. `session_start({engine, cwd, cols, rows})` validates the chosen directory, resolves the
owner-installed engine, launches it with no resume argument in that directory, and returns a console
whose `sessionKey` is null until discovery observes the new engine session. The command never writes
a Pigeon project record and never accepts an arbitrary executable or argv from the View.
**Tests.** T-34.1 cancel returns null; T-34.2 a chosen directory is normalized; T-34.3 each engine
starts with its bare program and no resume argument; T-34.4 missing engine or directory fails before
spawn; T-34.5 the console joins the newly discovered session when its source record appears.
**Traces.** UR-4, UR-6. **Origin.** owner.

### FR-35 Stop a session everywhere
**Statement.** `session_stop({key})` MUST terminate every process Pigeon can prove belongs to the
selected engine session, whether Pigeon launched it or discovered it externally.
**Behaviour.** The engine adapter supplies a session-specific process inventory and proof predicate.
The host refuses ambiguous matches, sends graceful termination, waits a bounded interval, then force
terminates remaining proven matches. Pigeon-owned consoles receive the normal exit event and are
removed from the visible terminal; source files are never modified. A stop of an already-stopped
session is an idempotent result with zero processes stopped.
**Tests.** T-35.1 fixture process inventory with one matching and one unrelated process stops only the
matching process; T-35.2 ambiguous process identity refuses without killing; T-35.3 graceful timeout
falls back to force termination; T-35.4 an external matching process disappears from status; T-35.5
stop leaves source mtimes and contents unchanged; T-35.6 closing the host terminates all owned PTYs.
**Traces.** UR-18. **Origin.** owner.

### FR-36 Configurable polling and Recent cutoff
**Statement.** Pigeon MUST expose validated `pollIntervalSeconds` and `recentWindowDays` settings.
**Behaviour.** Defaults are `5` seconds and `7` days. Bounds are `1..300` seconds and `1..365` days.
The poller schedules source reads at the configured interval. `recent` computes its cutoff as
`now - recentWindowDays`. A setting change invalidates current-scope views and schedules one refresh.
Polling MUST NOT write a database row, settings record, or source file on every tick; current-state
tables update only when a semantic value changes, while metrics and terminal scrollback remain
memory-cached until their source/lifecycle changes.
**Tests.** T-36.1 defaults and bounds; T-36.2 corrupt settings revert to defaults; T-36.3 changing
the interval changes the next poll deadline; T-36.4 changing the window changes Recent membership;
T-36.5 repeated unchanged polls produce no writes.
**Traces.** UR-19. **Origin.** owner.

---

## 4. Interface contract (view ↔ host)

All commands are Tauri `invoke`s; all payload keys camelCase; timestamps epoch ms UTC.

```ts
// host info
host_info(): { os: "macos"|"windows"|"linux"; arch: string; version: string }

// projects and sessions
sessions_list(args: { scope: "live"|"recent"; force?: boolean }): {
  scope: "live"|"recent"; sinceMs: number | null;
  rows: SessionRow[]; problems: EngineError[]; generatedAtMs: number }
session_metrics(args: { key: SessionKey }): MetricState
projects_summary(args: { scope: "live"|"recent" }): {
  scope: "live"|"recent"; sinceMs: number | null;
  projects: ProjectSummary[]; generatedAtMs: number }
project_pick(): { cwd: string | null }
session_start(args: { provider: ProviderId; cwd: string; cols: number; rows: number }): { id: string }
session_stop(args: { key: SessionKey }): StopResult

// account
account_status(args: { force?: boolean; provider?: ProviderId }): {
  accounts: Record<ProviderId, { identity: Identity; capacity: Capacity }>; generatedAtMs: number }

// status
status_snapshot(): StatusSnapshot

// console (frozen seam, minus dropped fields, plus engine)
console_open(args: { sessionKey: SessionKey; cwd: string; cols: number; rows: number }): { id: string }
console_ready(args: { id: string }): void
console_input(args: { id: string; dataB64: string }): void
console_resize(args: { id: string; cols: number; rows: number }): void
console_close(args: { id: string }): void
console_list(): { consoles: ConsoleSummary[] }

// hover + settings
hover_toggle(): { visible: boolean }
hover_select(args: SessionKey): void
settings_get(): Settings
settings_set(args: Partial<Settings>): Settings

// events (host → view)
"sessions://changed"  { generatedAtMs }
"sessions://metrics"  { rows: { key: SessionKey; metrics: MetricState }[]; generatedAtMs }
"status://changed"    StatusSnapshot
"capacity://changed"  { provider: ProviderId; account: AccountStatus; generatedAtMs: number }
"console://data"      { id; dataB64 }
"console://exit"      { id; exitCode: number | null }
"feather://select-session" SessionKey               // to the main window only
```

### 4.1 Types

```ts
type ProviderId = "claude-code" | "codex" | "opencode";
interface SessionKey { providerId: ProviderId; sid: string; }

interface SessionRow {
  key: SessionKey;
  cwd: string | null; project: string | null; projectLeaf: string | null;
  title: string; name: string | null; gitBranch: string | null;
  firstActiveMs: number | null; lastActiveMs: number;
  resumable: boolean; resumeBlockedReason: string | null;
  status: LiveStatus | null;          // null = no live process: no badge, not counted
  metrics: MetricState;               // pending, ready, or unavailable; never blank/zero
  sourceSummary: string | null;
  unknownTypes: Record<string, number>;
}
interface LiveStatus {
  state: "running" | "needs_you" | "finished" | "unknown";
  sinceMs: number | null; rawWord: string | null; evidence: string[];
  pid: number | null; consoleId: string | null;
}
type MetricState =
  | { state: "pending" }
  | { state: "ready"; value: Metrics }
  | { state: "unavailable"; error: EngineError };
interface Metrics {
  inputTokens: number; outputTokens: number; cacheRead: number; cacheWrite: number;
  apiCalls: number; toolCalls: number; userTurns: number; durationMs: number | null;
  reasoningTokens: number | null; providerCostUsd: number | null;
  kpis: { contextPerCall: number | null; rewriteRatio: number | null; batchingRatio: number | null };
  problem: EngineError | null;
}
interface ProjectSummary {
  project: string; projectLeaf: string; sessions: number; counted: number;
  statusCounts: { running: number; needsYou: number; finished: number; unknown: number };
  providers: Partial<Record<ProviderId, number>>; lastActiveMs: number;
  totals: Omit<Metrics, "kpis" | "basis" | "problem"> & { costRows: number };
  kpis: Metrics["kpis"];
}
interface Identity {
  provider: ProviderId; signedIn: boolean; label: string | null; organization: string | null;
  plan: string | null; tier: string | null; mode: string | null; accountShort: string | null;
  providers: { name: string; kind: string }[] | null; readAtMs: number; problem: EngineError | null;
}
interface Capacity {
  provider: ProviderId; supported: boolean;
  windows: { name: "five_hour" | "weekly"; windowMinutes: number; usedPct: number; resetsAtMs: number | null }[];
  plan: string | null; stale: boolean; sourceAgeS: number | null; reachedLimit: string | null;
  readAtMs: number; problem: EngineError | null;
}
interface StatusSnapshot {
  generatedAtMs: number;
  counts: { running: number; needsYou: number; finished: number; unknown: number };
  live: { key: SessionKey; projectLeaf: string | null; title: string | null;
          name: string | null; state: LiveStatus["state"]; sinceMs: number | null;
          evidence: string[]; pid: number | null; consoleId: string | null }[];
}
interface EngineError {
  provider: ProviderId | null;
  kind: "not_installed" | "root_missing" | "no_credential" | "credential_refused" | "transport"
      | "http_status" | "unknown_shape" | "stale" | "busy" | "io" | "unsupported" | "path";
  detail: { type: "none" } | { type: "status"; code: number } | { type: "exit"; code: number }
        | { type: "path"; path: string } | { type: "word"; word: string } | { type: "fields"; fields: string[] };
  message: string;   // from the catalogue, §9
}
interface ConsoleSummary { id: string; sessionKey: SessionKey | null; cwd: string;
  provider: ProviderId; mode: "resume" | "new"; running: boolean; exitCode: number | null; startedAtMs: number }
interface StopResult { key: SessionKey; stopped: { pid: number; evidence: string }[];
  alreadyStopped: boolean; ambiguous: { pid: number; reason: string }[] }
interface Settings { hover: { visible: boolean; corner: "tl"|"tr"|"bl"|"br"|null; x: number | null; y: number | null };
  list: { view: "live"|"recent" }; pollIntervalSeconds: number; recentWindowDays: number; split: number }
```

---

## 5. Data flow

1. **Launch.** `pathenv` builds the PATH (≤3 s). The view mounts; calls `host_info`,
   `settings_get`, `sessions_list`, `account_status`, `status_snapshot` in parallel. The list
   paints from heads; the status poller starts; the metrics worker starts filling newest-first.
2. **Steady state.** Pollers: status 3 s (Claude files + process table), OpenCode 5 s, Codex lsof
   10 s. The view refreshes the list on `sessions://changed`, chips on `sessions://metrics`, badges
   and rail/hover on `status://changed`.
3. **Resume.** Row → `console_open` → in-place terminal attaches (FR-10 order) → bytes flow → on exit
   `console://exit` → host refreshes sessions + status.
4. **Hover click.** `hover_select` → main shown/focused → `feather://select-session` → the list
   scrolls to and selects the row; if a console exists for it, the in-place terminal is focused.

---

## 6. Live status decision table

**The three owner-wait cases are one policy, resolved first.** Every engine implements
`adapters::wait::WaitPolicy` — `permission`, `question`, `interruption`, each returning a
`WaitSignal` or a stated `None` — and the provided `owner_wait()` applies the precedence
**permission → question → interruption**, with `Permission`/`Question` → **needs you** and
`Interruption` → **finished**. Only when no owner case is outstanding does the ordinary
running/waiting/unknown turn call below apply. The precedence and the trait are recorded in
`docs/decisions/0003-wait-policy.md`.

Evaluated per `(engine, sid)` in order; the first matching row decides.

| # | Evidence | State | since |
|---|---|---|---|
| 1 | pigeon hosts a console for it, `running: true`, and engine evidence says nothing | **running** | console start |
| 2 | pigeon hosts a console, `running: true`, engine evidence present | engine evidence (rows 3–9) | engine's |
| 3 | Claude file, pid alive, `status ∈ {busy, running}` | **running** | `statusUpdatedAt` |
| 4 | Claude file, pid alive, `status ∈ {needs_input, blocked, permission, waiting}` | **needs you** | `statusUpdatedAt` |
| 5 | Claude file `idle`, terminal assistant `tool_use` unanswered and named `AskUserQuestion` / `ExitPlanMode` | **needs you** | block `ts` |
| 5b | Claude file, pid alive, `status == idle` | **finished** | `statusUpdatedAt` |
| 6 | Claude file, pid alive, any other word | **unknown** (`rawWord` shown) | `statusUpdatedAt` |
| 7 | Codex process attached, rollout turn open (`task_started`), and the installed `PermissionRequest` hook is the newest event for the thread | **needs you** | event ts |
| 7b | Codex process attached, tail last turn event `task_started`, or a `user_message` after a closing event | **running** | newest work record ts |
| 8 | Codex process attached, tail `task_complete` / `turn_aborted` / `error` | **finished** | event ts |
| 9 | Codex process attached, no turn event found | **unknown** ("no turn event in tail") | process start |
| 10 | OpenCode process in the directory, this is its newest session, a pending approval in the event stream or a `permission` row for its project | **needs you** | pending event, else permission `time_created` |
| 10b | OpenCode process, newest session, newest part is a `question` tool `running`/`pending` | **needs you** | part `time_updated` |
| 11 | OpenCode process, newest session, newest assistant message `time.completed == null` and no owner case outstanding | **running** | message `time.created` |
| 12 | OpenCode process, newest session, newest assistant message completed, or `MessageAbortedError` on it | **finished** | `time.completed` |
| 13 | pigeon hosted a console for it and it exited | **no status** (`null`) | — |
| 14 | no live evidence of any kind | **no status** (`null`): no badge, not counted | — |
| 15 | the process table could not be read | **unknown** ("process table unavailable") for every row that would otherwise be status-less | — |

Counts in rail and hover: `running`, `needsYou`, `finished` from `live`; `unknown` shown only when
non-zero. Status-less rows are never counted (owner ruling 2026-09-12: "no point in counting done
ones, there could be a lot of them, user won't care").

**No aging bound while a process is alive.** The studio ages every open observation out at
`lineage_timeout_seconds` (default 1200 s, the owner's box 900 s) because it has no process
evidence — a transcript alone cannot tell a crashed turn from a long one. Pigeon has the process,
so a running row stays running while its pid lives and shows "quiet Nm" once the newest record is
older than 20 minutes; a dead pid drops the status at once. The studio's one exemption — never time out a
human gate (a 21.8 h `waiting-user` was observed) — is moot for the same reason: needs-you lasts as
long as the process does.

**Relationship to the studio's vocabulary.** Studio `live_signal ∈ {waiting-user, open-tool,
open-turn, open-background, none, unknown}` and disposition `∈ {live, finished, timed out, cut off,
unknown, waiting}`. Pigeon's **running** ≈ studio `open-tool | open-turn | open-background`;
**needs you** ≈ studio `waiting-user` plus a hosted session's open approval gate; **finished** is
what the studio deliberately renders as *not live* (owner ruling quoted in
`adapters/codex/parser.py:98-101`: "idle if not working would not light the session active") —
pigeon keeps that ruling by never counting your-turn as running and by listing it below running in
the hover; a status-less row ≈ studio `finished` with no process; **unknown** ≈ `unknown`. Studio's `timed out` and `cut off` have no
pigeon equivalent because they exist only to age observations without process evidence.

---

## 7. UI specification

### 7.1 Layout (main window, default 1280 × 800, minimum 1100 × 680)

```
┌────────────────────────────────────────────────────────────────────────────┐
│ AccountStrip: [Claude card] [Codex card] [OpenCode card]      ⟳  hover ◐  ⓘ│  ≈ 96 px
├───────────────────────────┬────────────────────────────────────────────────┤
│ StatusRail ● 1 running    │ RightPane                                      │
│  ● 1 needs you ◐ 3 finished│   │  ProjectSummary / SessionDetail          │
│  [Live] [Recent · 7 days] │   │  Metrics → ConsoleView (in place)         │
├───────────────────────────┤   │                                            │
│ ProjectList (virtualised) │   │                                            │
│  project ▾                │   │                                            │
│    session · session …   │   │                                            │
│                           │   └────────────────────────────────────────────┘
└───────────────────────────┴────────────────────────────────────────────────┘
        ≈ 38 % (drag to resize, 320 px min)          ≈ 62 % (480 px min)
```

### 7.2 Session row (56 px)

Line 1: `StatusBadge` · `EngineBadge` · **project leaf** · title (ellipsised) · right-aligned
last-active. Line 2: `MetricChips` (`1.2M tok` = input+output+cache read+cache write · `84 calls` ·
`61 tools` · KPI trio `ctx 22k · rw 0.09 · batch 1.8`) or `counting…`; a `name` tag when the
engine names the session; a **Resume** button appearing on hover/focus (disabled with a tooltip when
not resumable). Selected row highlighted; keyboard ↑/↓ moves selection, Enter opens detail, ⌘↩
resumes.

### 7.3 Project card (primary item, 96 px)

`▸/▾` · **leaf** · normalized path · live/session counts · engine badges · `counted n/m` · summed
chips · KPI trio · last active. Live expands to live sessions; Recent expands to sessions active in
the last seven days. Actions are Open folder and Add session (Claude/Codex/OpenCode). Clicking the
card selects the project; a session row selects session detail.

### 7.4 Number formatting

Tokens: `<1,000` as digits; `<100,000` → one decimal `k` (`84.2k`); `<1,000,000` → integer `k`
(`842k`); else one decimal `M`/`B`. Counts (calls, tools, turns) as digits with thousands
separators. `contextPerCall` as tokens (`22k`); `rewriteRatio` two decimals; `batchingRatio` one
decimal; absent KPI `—`. Percent integer. Cost `$0.0040` (four decimals under $1, two above),
always with "engine-reported".

### 7.5 Badges and colours

| State | Label | Colour token |
|---|---|---|
| running | running | `--live` (green) with a subtle pulse |
| needs_you | needs you | `--hot` (red) |
| finished | finished | `--warn` (amber) |
| unknown | unknown | `--muted` outlined, tooltip with the missing signal |
| (null) | — | no badge; the row shows only its last-active time |

Engine badges: `claude` · `codex` · `opencode`, monochrome text badges. The theme follows the
system (light/dark) with explicit tokens; no colour is the only carrier of meaning.

### 7.6 Empty, loading, error states

Loading: skeleton rows for ≤ 400 ms, then content. Empty provider: no rows, a strip chip. Empty
everything: "no sessions found under ~/.claude, ~/.codex, ~/.local/share/opencode". Problems: a
collapsed "n problems" line above the list expanding to the catalogue sentences.

### 7.7 The hover (320 × 240)

```
┌─────────────────────────────┐
│ ●1 run ●1 needs you ◐3 fin  │  header, drag region
├─────────────────────────────┤
│ needs you · claude · studio │
│   session-console-monitor 2m│
│ running · codex · dmi       │
│   Add the atlas legend   14m│
│ finished · claude · pyd-ai  │
│   pydantic-ai-3e         1d │
│ …                           │
├─────────────────────────────┤
│ +3 more          09:14:02   │
└─────────────────────────────┘
```

---

## 8. Non-functional requirements

- **NFR-1 Performance budgets (release build, this Mac).** First paint ≤ 1.0 s from launch;
  `sessions_list` cold ≤ 300 ms for 500 sessions across engines, warm ≤ 5 ms; a status tick ≤ 50
  ms CPU; hover and badges reflect a Claude status change ≤ 5 s; a console byte round-trip ≤ 30 ms;
  RSS ≤ 150 MB with 1,000 sessions and 3 consoles. The metrics worker never blocks a command.
- **NFR-2 Read-only.** No engine file is written, renamed, truncated, locked for writing or
  touched (mtime unchanged). SQLite is opened `mode=ro`, never `immutable`. Verified by T-4.3 and
  by a test that hashes every fixture root before and after a full run.
- **NFR-3 Privacy.** Local-only; the single network destination is `api.anthropic.com/api/oauth/
  usage`; no analytics, no crash reporting, no update check.
- **NFR-4 Fail loud on unknown shapes.** Every parser is total over the shapes in §2 and produces
  a catalogued absence for anything else; no `unwrap_or(0)` on a missing field that feeds a number.
- **NFR-5 UTC inside, local at the edge** (FR-31).
- **NFR-6 Portability.** `cargo check --all-targets` and `cargo clippy --all-targets -D warnings`
  pass for `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` in CI; the
  view has no platform branch except through `host_info`.
- **NFR-7 The owner's CLIs, never bundled.** The bundle contains no engine binary; PATH resolution
  at spawn; a missing engine is a stated absence.
- **NFR-8 Test coverage.** Every parser rule in §2 has a fixture-backed unit test; every
  `EngineError` kind has a serialisation test; the leak table runs in CI; vitest covers formatting,
  ordering, the attach order and the hover; the manual macOS smoke checklist (§10.3) is recorded
  per release in `docs/verification/`.
- **NFR-9 Zero setup.** `git clone && npm install && npm run tauri dev` on a machine with Rust,
  Node and Xcode command-line tools; no Python, no environment variables, no test vault.
- **NFR-10 Accessibility basics.** Keyboard navigation of projects, sessions, buttons and the
  embedded terminal; visible focus;
  labels on every badge; contrast ≥ 4.5:1 for text in both themes; the terminal announces nothing
  (xterm's own a11y is out of scope).
- **NFR-11 KPIs are diagnostics.** No score, grade, rank or target wording anywhere; no export of
  numbers to a brief format.
- **NFR-12 Log hygiene.** Log ≤ 15 MB total; no credential, no transcript text, no prompt text;
  paths are allowed.
- **NFR-13 Process-control safety.** Stop operations are session-scoped, engine-specific, bounded,
  auditable, and fail closed on ambiguous identity. Pigeon never kills a process merely because its
  cwd or executable name looks similar; it must have session-specific evidence.
- **NFR-14 Hot-state write discipline.** Polling is read-heavy. Pigeon MUST NOT persist a row on
  every poll tick, terminal byte, or unchanged observation. It updates current-state tables only on
  semantic changes, batches/coalesces writes, and keeps high-frequency metrics/scrollback in memory.

---

## 9. Error catalogue

| kind | detail | sentence shown |
|---|---|---|
| not_installed | word=program | "`<program>` is not on this machine's PATH" |
| root_missing | path | "no <engine> sessions on this machine (<path> not found)" |
| no_credential | none | "no Claude Code credentials on this machine" |
| no_credential | exit=44 | same as above |
| credential_refused | status=401/403 | "the usage endpoint refused the stored login (HTTP <n>) — it likely expired; start a new `claude` session to refresh it" (+ expiry clause) |
| credential_refused | exit=n | "the Keychain refused the read (security exit <n>)" |
| transport | word=class | "could not reach the usage endpoint (<class>)" |
| http_status | status=429 | "the usage endpoint answered HTTP 429 — retry after <n> s" |
| http_status | status=n | "the usage endpoint answered HTTP <n>" |
| unknown_shape | none | "the usage endpoint did not answer JSON" / "…did not answer an object" (by branch) |
| unknown_shape | fields | "the <source>'s shape has changed: <fields>" |
| unknown_shape | word=no-window | "the usage endpoint reported no 5-hour or weekly window" / "the Codex rate-limit record named no 5-hour or weekly window" |
| unsupported | word=reached | "Codex reports a reached limit: <word>" |
| unsupported | none | "capacity not exposed by this engine" |
| stale | none | "reading from <time>, stale" (flag, not a failure) |
| busy | none | "OpenCode's database was busy; retrying" |
| io | path | "could not read <path>" |
| path | path | "<path> no longer exists" (resume blocked) |

---

## 10. Verification plan

### 10.1 Rust unit tests (fixtures under `src-tauri/tests/fixtures/{claude,codex,opencode,auth}/`)
As enumerated per FR (`T-##.#`). Fixtures are redacted copies: the studio's
`tests/capture/samples/claude_oauth_usage_200.json` and `codex_token_count_*.jsonl`, plus new
minimal transcripts (a streamed three-record message; a subagent restating the parent's id; an
`ai-title`; a tool_result-first user record; a compact summary), Codex heads for the four wrapper
kinds and a `user_message`, a Codex resume pair and a subagent file, and an OpenCode WAL database
generated by the test with `rusqlite`.

### 10.2 vitest
Formatting table (FR-31, §7.4); list ordering and grouping; `ConsoleView` attach order; hover
ordering and cap; badge labels; `windowsPty` gating; `MetricChips` pending vs absent vs values.

### 10.3 Manual macOS smoke (recorded per release)
1. `npm run tauri dev`: strip shows three cards with real readings; list shows real rows; badges
   show the currently live sessions and agree with `claude agents --json`.
2. Resume a Claude row → banner ≤ 2 s → type `/exit` → exit notice with code → the row loses its
   badge. Repeat for a Codex row (`/quit`) and an OpenCode row (`ctrl-c` twice).
3. In another Claude session, trigger a permission prompt → pigeon badge and hover show **needs
   you** ≤ 5 s → answer → **running** → **finished**.
4. Toggle the hover, drag it, quit, relaunch: same position and visibility; it stays above a
   full-screen Terminal.
5. `npm run tauri build`, open the `.app` from Finder: steps 1–2 again (the PATH test).
6. Cross-check: three Claude sessions' six counters equal the studio's `sessions.detail`.
7. Secret grep: run with `RUST_LOG=debug`, grep the log and the WebView console for the first 8
   characters of the Keychain secret (read once by the tester with `security … -w | cut -c1-8`) →
   zero hits.

### 10.4 Performance check
500 synthetic Codex heads + 500 Claude heads + 500 OpenCode rows in fixture roots; release build;
`sessions_list` cold ≤ 300 ms; status tick ≤ 50 ms.

---

## 11. Traceability (FR → UR)

| FR | UR | | FR | UR |
|---|---|---|---|---|
| FR-1 | 3, 15 | | FR-18 | 5, 6 |
| FR-2 | 3 | | FR-19 | 7 |
| FR-3 | 3 | | FR-20 | 7 |
| FR-4 | 3, 11, 15 | | FR-21 | 7, 15 |
| FR-5 | 1, 3 | | FR-22 | 7, 9 |
| FR-6 | 1, 5, 17 | | FR-23 | 8 |
| FR-7 | 6 | | FR-24 | 1, 9 |
| FR-8 | 4 | | FR-25 | 2, 3, 10 |
| FR-9 | 4 | | FR-26 | 2, 13 |
| FR-10 | 1, 4 | | FR-27 | 14 |
| FR-11 | 2, 12 | | FR-28 | 4, 14 |
| FR-12 | 2, 13 | | FR-29 | 12 |
| FR-13 | 2, 13 | | FR-30 | 11 |
| FR-14 | 2, 13 | | FR-31 | 2, 3 |
| FR-15 | 1, 2, 17 | | FR-32 | 10, 14 |
| FR-16 | 5, 13 | | FR-33 | 12 |
| FR-17 | 5, 6, 13 | | FR-34 | 4, 6 |
| FR-35 | 18 | | FR-36 | 19 |

---

## 12. Open questions

- **OQ-1 Claude subagent inclusion.** §2.1.6 folds `<uuid>/subagents/*.jsonl` into the session's
  counters with global id dedupe. T-16.1 against the studio decides whether the studio's
  `sessions.detail` figure includes subagents; if not, pigeon shows two lines ("session" and
  "incl. subagents") rather than silently picking one.
- **OQ-2 Codex `total_token_usage` across resumes.** Whether it is cumulative per thread or per
  file is untestable here (no resume pair exists on this Mac). Pigeon sums per-call deltas, which
  is correct either way; T-16.3 checks the per-file invariant.
- **OQ-3 Codex "needs you".** No approval event appears in rollouts sampled here, and the studio
  found none in 142 rollouts (`adapters/__init__.py:59-62, 130-142`: "Codex records no
  approval-pending state"). If the owner sees a blocked Codex shown as "finished", the only known
  evidence is the app-server's `item/commandExecution/requestApproval` /
  `item/fileChange/requestApproval` for sessions pigeon itself hosts — a second-pass item that would
  make pigeon's own console the source for Codex gates.
- **OQ-4 OpenCode `permission` semantics.** Zero rows at measurement; confirm at first occurrence
  that a pending prompt inserts a row and an answer deletes it.
- **OQ-5 Claude status words.** Only `busy` and `idle` observed; `needs_input` documented. The
  binary also contains `running`, `blocked`, `permission`, `waiting`, `exited`. The mapping in §6
  rows 3–6 is to be revisited after the first week of use; unknown words are shown verbatim.
- **OQ-6 Long-idle sessions.** An 18-day `idle` session is technically "finished". Whether the
  hover should demote your-turn rows older than N hours is an owner call after use.

---

## Appendix A — Verified facts, this Mac, 2026-09-12

- Binaries: `~/.local/bin/claude` (2.1.269), `~/.bun/bin/codex` (0.149.1), `/opt/homebrew/bin/
  opencode` (1.18.29). `launchctl getenv PATH` unset.
- Claude: 19 project dirs, 50 sessions, 45 subdirs; first `cwd` by line 7 (attachment records),
  first user text by line 10, in 50/50; `~/.claude/.credentials.json` absent; Keychain item
  `Claude Code-credentials` present; `~/.claude.json` has `oauthAccount` and a stale
  `cachedUsageUtilization` (a day old while in use on 2026-09-10 — not read). `~/.claude/sessions/`
  held 4 live process files (statuses busy ×1, idle ×3) and their `.key` twins; `claude agents
  --json` returned the same 4 in 182 ms. `ai-title` records present (180 in the 5 newest files).
  Assistant `stop_reason` values seen: `tool_use` 827, `end_turn` 92, null 12, `stop_sequence` 2.
- Codex: 71 rollouts; 4 subagent files; `session_index.jsonl` 12 lines; `thread-writer-locks/`
  held one `<thread>.lock` for the one live `codex resume`; that process had the rollout (fd 41) and
  the lock (fd 42) open; `event_msg` types in the 10 newest: `task_started`, `item_completed`,
  `token_count`, `task_complete`, `thread_settings_applied`, `turn_aborted`; `response_item` types
  in the newest: `message`, `reasoning`, `custom_tool_call`, `custom_tool_call_output`;
  `token_count.payload.info` keys: `last_token_usage`, `total_token_usage`, `model_context_window`;
  live `rate_limits`: primary 300 min 35 %, secondary 10080 min 6 %, `plan_type: team`. Three
  `codex app-server` processes (VS Code) alive.
- OpenCode: `opencode.db` 38 MB WAL; 28 sessions (5 children); 27 with tokens, 25 with cost;
  message roles assistant 916 / user 207; part types step-start 889, tool 884, step-finish 873,
  reasoning 834, text 442, patch 179, file 8, subtask 5; `step-finish` parts carry `tokens` and
  `cost`; `permission` table empty; `account` table empty; two live `opencode` processes.
- Cargo lock of the studio (reference): `reqwest 0.13.4` present **without TLS**; `rusqlite`
  absent; `chrono`, `time`, `dirs` present.
- The studio reads **no process facts** for externally started sessions (a repo-wide search for
  `psutil`, `tasklist`, `lsof`, `/proc`, `ps` returns nothing; `app/sidecar/proctree.py` is a
  56-line tree-kill helper). Its liveness is transcript-tail-only with a 1200 s aging bound. Pigeon's
  status files, process table and `lsof` mapping are new evidence, which is why pigeon can show
  **finished** and "no process" as facts rather than as aged-out inferences.

## Appendix B — Provenance (studio → pigeon)

| Pigeon rule | Studio source (branch `mac-first-launch-fixes-2026-09-11`) |
|---|---|
| PTY runtime, registry, gate, pump, resolve, argv | `app/src-tauri/src/console.rs` (production body 1–1242) |
| Terminal surface and attach order | `app/src/console/ConsoleView.tsx`, `terminalEngine.ts`, `consoleTheme.ts`, `app/src/platform/consoleTransport.ts`, `hostEvents.ts` (commit `2df9d379`) |
| Head/first-user extraction and exclusions | `src/demo_studio/capture/session_meta.py:10-21, 712-734` |
| Claude fold (dedupe by id, elementwise MAX) | `src/demo_studio/adapters/claude_code/parser.py` ~1258; `planning/project-seed.md §2.3` |
| Compact-summary marker | `adapters/claude_code/records.py:723, 821-852` |
| Codex identity and grouping | `src/demo_studio/capture/discovery.py:122-243`; ADR-0046 |
| Codex usage mapping (cached subtracted) | `adapters/codex/parser.py:705-724`; ADR-0016 decision 2 |
| Codex rate limits (minutes, sibling of info, stale) | `capture/usage_capacity.py:291-301, 1873-1999`; ADR-0043 |
| Claude token load, Keychain, usage parse, reasons | `capture/usage_capacity.py:153-238, 598-678, 838-993`; ADR-0043 amendment 2026-09-10 |
| Claude identity keys | `capture/usage_capacity.py:1428-1468` |
| KPI formulas | `src/demo_studio/derive/formulas.py:42-47, 94-102` |
| Lock discipline | `CLAUDE.md` invariant 10; `console.rs` header |
| Widget precedent (always-on-top plate) | `app/src-tauri/src/widget_win.rs`, `app/src/telemetry/TelemetryPlate.tsx`, `plateModel.ts`, `useTelemetryLanes.ts:37-58`, `main.rs:1734-1772`, `capabilities/telemetry-widget.json`, commit `ce3e2da6` (`macos-private-api`) |
| Corner docking geometry (344×252 plate, 12 px margin, work-area rect, resize-then-move) | `app/src/telemetry/dock.ts:28-46, 281, 1156-1206` |
| Live-status vocabulary and the "idle is not live" ruling | `src/demo_studio/model.py:1527-1685`, `derive/liveness.py:33-54, 303-482`, `derive/session_report.py:190-288, 396-528`, `adapters/codex/parser.py:98-131, 1230-1280` |
| Claude tail rule and the two human-gate tools | `adapters/claude_code/parser.py:2040-2181`, `adapters/__init__.py:64-76` |
| Subagent files count toward last-active | `capture/watcher.py:48-64`, `store/schema.py:49-54` |

## Appendix C — Glossary
See URD §9. Additionally: **head read** — reading the first bytes of a file up to a budget;
**tail read** — reading the last bytes by seeking from the end; **fold** — the full-file counting
pass; **signature** — the `(path, size, mtime)` tuple that keys the metrics cache; **snapshot** —
one evaluation of the live-status decision table; **hosted** — a session whose process pigeon
spawned in a console.
