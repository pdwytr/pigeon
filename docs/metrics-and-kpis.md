# What Pigeon counts, and how to read it

**Status:** explainer · **Date:** 2026-09-15 · **Normative sources:** `contracts/frds.md` §2.1.6,
§2.2.5, §2.3.3, FR-16, FR-17 · `contracts/data.md` · `contracts/objects.md` §4.4 ·
`decisions/0001-implementation-rulings.md`

This is prose around numbers that are specified elsewhere. Where this article and the contracts
disagree, the contracts win — with the two exceptions noted at the end, which are places where the
*code* and the contracts already disagree.

---

## 0. The one rule that governs all of it

> **These numbers are owner diagnostics, never agent targets.** (`CLAUDE.md` invariant 4)

Every counter below describes what an engine already did. None of them is a score, none has a target
value, and none should be handed to an agent as an objective — a batching ratio optimised *for* is a
batching ratio that no longer measures anything. They exist to answer "where did my week go" and
"which session is worth reopening".

The second rule is about absence:

> **Null, pending, unavailable and zero are four different things, and the view renders four
> different things.** (invariant 6)

A session that made no API calls and a session Pigeon could not read look identical the moment
either one is allowed to render `0`. They are opposite conclusions about whether to go and look at
it, so the product never collapses them.

---

## 1. The counters

Nine fields land in `Metrics` (`src-tauri/src/domain/metrics.rs`). Six of them — **the raw six** —
are the ones every surface shows and the ones the KPIs are built from. Three more are carried
because the detail pane wants them, and a fourth is carried only when an engine states a price.

| Field | Type | Meaning | Absent when |
|---|---|---|---|
| `inputTokens` | `u64` | **Uncached** prompt tokens sent | never — a real 0 is a real 0 |
| `outputTokens` | `u64` | Tokens the model generated | never |
| `cacheRead` | `u64` | Prompt tokens served from cache | never |
| `cacheWrite` | `u64` | Tokens written *into* cache | never |
| `apiCalls` | `u64` | Distinct model calls | never |
| `toolCalls` | `u64` | Distinct tool invocations | never |
| `userTurns` | `u64` | Times a human actually typed | never |
| `durationMs` | `Option<u64>` | Last timestamp − first | no timestamps, or a clock that ran backwards |
| `reasoningTokens` | `Option<u64>` | Thinking tokens, a **subset** of output | the engine stated none |
| `providerCostUsd` | `Option<f64>` | The **engine's own** dollar figure | the engine states no price |

Three arithmetic conventions hold across all three engines, and each one exists because getting it
wrong produces a plausible number rather than an obvious error:

**`inputTokens` means uncached input.** Claude's API reports input and cache-read as disjoint
quantities, so that is the meaning Pigeon adopts everywhere. Codex reports a superset and is
converted; see §2.2.

**Session tokens are `input + output + cacheRead`** (`src/format.ts:totalTokens`). `cacheWrite` is
deliberately excluded: a cache write is the cost of *putting* content into the cache, and adding it
to the reads of that same content counts the same context twice.

**`reasoningTokens` is never an addend.** It is a subset of `outputTokens` and is displayed beside
them, never summed into them.

**Pigeon never invents a price.** Claude and Codex are subscription logins here, so a per-token
cost would be fiction. `providerCostUsd` carries an engine's own figure or nothing at all — and
nothing renders as an omitted chip, not as `$0.00`, which would read as "this session was free".

---

## 2. Where each engine's numbers actually come from

The three engines expose completely different surfaces, and `MetricBasis` records which one
produced a given row. It is diagnostic only — it changes no number — but it tells you how much to
trust a figure and what would have to break for it to be wrong.

### 2.1 Claude Code — `MetricBasis::Fold`

**Source.** `~/.claude/projects/<encoded-dir>/<uuid>.jsonl`, one depth-1 UUID stem per session.
Measured on the owner's Mac, 2026-09-13: 18 project directories, 51 transcripts, 21,033 records,
largest file 14.6 MB.

**The counting rule, which is frozen.** API calls are deduped by `message.id`, and within one id
every usage key takes the **elementwise MAX** across the records sharing it. The four keys read are
`input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`.

This is not a style preference. `output_tokens` is a *streaming counter* that grows line by line as
one message is written, so summing the logged records double-counts by 2–6× (measured in Demo Studio) and reading the first occurrence under-counts. MAX is the final value.

The same identity rule carries `tool_calls`: a `tool_use` block restated on a later line of the same
streamed message is one call. It also absorbs, for free, the shape that broke Studio twice in
September 2026 — a forked subagent's transcript restating its parent's dispatching record byte for
byte, same `message.id`, same `usage`, same `tool_use` block. Deduping by id counts it once.

| Counter | Derivation |
|---|---|
| `apiCalls` | count of distinct `message.id` |
| the four token fields | per-id MAX, then summed across ids |
| `toolCalls` | size of the union of `content[].id` where `type == "tool_use"` |
| `userTurns` | `user` records that are not plumbing |
| `durationMs` | last `timestamp` − first, in **file order** |
| `reasoningTokens` | `usage.output_tokens_details.thinking_tokens`, per-id MAX then summed |
| `providerCostUsd` | always absent — subscription login |

**What makes it fail loud.** Exactly two things, both of them "this number would otherwise be a
guess": an `assistant` record with no `message.id` (there is no dedupe key, so any count is
invented), and a `usage` object in which none of the four frozen keys appears (the schema moved). An
unrecognised record *type* is not one of them — it is tallied in `Diagnostics` and the rest of the
file is still read, because a new type the engine added says nothing about the records we do
understand.

**One subtlety worth knowing.** `reasoningTokens` is `None` — not `Some(0)` — when no record stated
the field, and `Some(0)` when a record stated a zero. Those are different facts. An older CLI build
states nothing here, and a 0 would be Pigeon asserting "this call did no thinking" about a record
that said no such thing.

### 2.2 Codex CLI — `MetricBasis::Deltas`

**Source.** `~/.codex/sessions/**/rollout-*.jsonl`. Measured 2026-09-13 against Codex 0.149.1: 72
rollout files, 68 logical sessions, 3,208 `token_count` events, largest single line 1.52 MB.

**A session is a group of files, not a file.** A resume writes a *new* rollout for the same thread,
so the counters scan every file in the group. The sid is the whole UUID, never a prefix: these are
UUIDv7 and the first 8 hex characters advance only every ~65 seconds.

**Token totals are taken, not summed.** `payload.info.total_token_usage` is cumulative over the
thread, so the *last* `token_count` event already is the session's total. Adding the events would
multiply the answer by the number of turns.

| Counter | Derivation |
|---|---|
| `inputTokens` | `input_tokens − cached_input_tokens`, saturating at 0 |
| `cacheRead` | `cached_input_tokens` |
| `cacheWrite` | `cache_write_input_tokens` |
| `outputTokens` | `output_tokens` |
| `reasoningTokens` | `reasoning_output_tokens` (always `Some`, including `Some(0)`) |
| `apiCalls` | count of `token_count` events |
| `toolCalls` | `response_item`s of type `function_call`, `custom_tool_call`, `local_shell_call` |
| `userTurns` | `user_message` events, or — per file, when a file states none — wrapper-filtered user `response_item`s |
| `durationMs` | max − min `timestamp` across the group |
| `providerCostUsd` | always absent — subscription login |

**The subtraction is a units conversion, not a correction.** Codex's `input_tokens` is a superset
that already contains `cached_input_tokens`: one session on this Mac reported 12,208,937 input
tokens of which 11,544,064 were cached, and `total_tokens = input + output` exactly. Passing that
figure straight through would double-count the cached prompt — once under `inputTokens`, again under
`cacheRead` — *and* make a Codex row incomparable with a Claude row in the project card that sums
them. Ruling 1 in `decisions/0001`. Do not "fix" it back.

**The user-turn fallback.** 22 of the 72 rollouts here emit no `user_message` event at all — the
paginated-history shape, which includes all three of the newest sessions. Counting only events
renders 0 turns for sessions that plainly had them. The fallback is decided **per file**, so a
resume that states events never suppresses an earlier file's fallback, and the two are never added
(the response items are the same words restated).

**A group with no `token_count` at all is not a failure.** 12 of the 68 sessions here are real
zero-call sessions — the owner opened Codex and closed it. They return the tool and turn counts that
were positively parsed with the token counters and `apiCalls` at 0, and `apiCalls == 0` makes every
KPI `None`, so the view states an absence rather than drawing a zero bar. An *unreadable* file, by
contrast, is an error.

**What makes it fail loud.** `cached_input_tokens > input_tokens` — because that is the relationship
the subtraction rests on, and 0 of 3,173 records here breach it, so one that does is news. Also an
`info` that is present but not an object, and any usage field present but not a non-negative
integer. Absences are ordinary and must not fail: 35 of 3,208 events carry no `info` at all, and
`cache_write_input_tokens` is absent from 1,504 of the 3,173 that do.

### 2.3 OpenCode — `MetricBasis::Columns`

**Source.** `~/.local/share/opencode/opencode.db` — one SQLite database, 59 MB with a 3.8 MB WAL
beside it on this Mac, read live and read-only while the engine writes to it. Measured 2026-09-13:
29 `session` rows (24 top-level and unarchived, 5 subagent children), 1,536 `message` rows, 5,785
`part` rows.

**Counting is a column read, not a fold**, because OpenCode already totals tokens and its own dollar
cost per session.

| Counter | Derivation |
|---|---|
| `inputTokens` | `session.tokens_input` |
| `outputTokens` | `session.tokens_output` |
| `cacheRead` | `session.tokens_cache_read` |
| `cacheWrite` | `session.tokens_cache_write` |
| `reasoningTokens` | `session.tokens_reasoning` (always `Some`) |
| `apiCalls` | `message` rows whose `data` JSON has `role == "assistant"` — see §6 |
| `userTurns` | `message` rows whose `data` JSON has `role == "user"` |
| `toolCalls` | `part` rows whose `data` JSON has `type == "tool"` |
| `durationMs` | `time_updated − time_created` (both epoch **milliseconds**, unlike the other two engines) |
| `providerCostUsd` | `session.cost` — **the engine's own figure, passed through untouched** |

`message` has no `role` column; the role lives in its `data` JSON, so the role test is a
`json_extract` guarded by `json_valid` — an unguarded `json_extract` on one non-JSON row aborts the
whole statement and would take 23 innocent sessions' counts down with it. Rows that fail the guard
are *counted* as drift, not ignored.

The counts are two grouped statements, never one per session: N+1 would be 48 extra statements fired
at a live writer for numbers two `GROUP BY`s already have.

**OpenCode is the only engine that states a price**, and it is the only place a dollar figure in
Pigeon comes from.

---

## 3. The three KPIs

Three ratios, ported verbatim from Demo Studio's `derive/formulas.py` so the two products agree
on every number. They are frozen: changing a definition needs a decision record.

```text
context_per_call = cache_read  ÷ api_calls
rewrite_ratio    = cache_write ÷ cache_read
batching_ratio   = tool_calls  ÷ api_calls
```

They are computed in Rust and never in the view. `KpiChips.tsx` does `toFixed` and nothing else — if
a division ever appears in that file, the boundary has been broken, and two surfaces can start
disagreeing about what "rewrite ratio" means.

They are **not stored**. `data.md` leaves them out of the schema on purpose and they are recomputed
at projection time, so there is no second copy to drift.

### Context per call — `cacheRead ÷ apiCalls`

**What it measures.** How much cached context each model call carried. In practice: how heavy the
conversation had become by the time the engine made a turn.

**How to read it.** Rendered as tokens (`112K`). It climbs through a session's life as the
transcript grows, and it climbs faster when the session is loaded with large files, long tool
outputs, or a big system prompt. A session with a high context-per-call is not doing anything wrong
— it is telling you that each additional turn in that session is expensive, and that a fresh session
would be cheaper for unrelated work.

**What makes it undefined.** `apiCalls == 0` — a session that never reached a model. That renders as
`—`, never `0`.

**What it is not.** It is not the context *window* usage: the denominator counts calls, not
messages, and the numerator counts cache reads only, not the uncached input that rides alongside.

### Rewrite ratio — `cacheWrite ÷ cacheRead`

**What it measures.** Stale-resume churn — how much of the cached context had to be written again
rather than read back. A cache write happens when the prefix the engine wanted was not there to be
read.

**How to read it.** Two decimals. **Low is good.** A ratio near zero means the session kept hitting
a warm cache. A high ratio means the session repeatedly paid to rebuild context it had already
built, which is the signature of resuming a session after its cache expired, or of something
invalidating the prefix on every turn.

**What makes it undefined.** `cacheRead == 0`. Note this is the one KPI whose denominator is not
`apiCalls`: a session can have made plenty of calls and still have an undefined rewrite ratio.

**A measured caveat you need before reading a Codex row.** In the current Codex corpus on this Mac,
`cache_write_input_tokens` is absent from 1,504 of 3,173 usage-bearing events, and the audit in
`frds.md` §2.4 records **no positive cache-write signal at all** for Codex. A Codex rewrite ratio of
`0.00` therefore means "this engine did not report cache writes", not "this session had perfect
cache behaviour". The audit's own decision requires that Codex's rewrite ratio be labelled as a
current zero/no-positive-signal measurement rather than implying Codex emits the same cache-write
behaviour as Claude or OpenCode.

### Batching ratio — `toolCalls ÷ apiCalls`

**What it measures.** Parallel-call discipline: how many tools the agent invoked per model turn.

**How to read it.** One decimal. A ratio near 1.0 means one tool per turn — a strictly serial agent,
paying a full round trip for each file it reads. Above 1.0 means turns are issuing several tool
calls at once. Below 1.0 means many turns did no tool work at all: conversation, planning,
clarification.

There is no target. A planning-heavy session *should* sit low; a session doing a wide sweep of file
reads *should* sit high. It is a shape cue, not a grade — and the moment it becomes a grade, see §0.

**What makes it undefined.** `apiCalls == 0`.

**A real zero is not an absence.** `apiCalls: 4, toolCalls: 0` produces `Some(0.0)` and renders
`0.00`. The session made four calls and used no tools; that is a measurement. This is pinned by
`a_real_zero_is_not_an_absence` in `domain/metrics.rs`.

### Project KPIs

A project's KPIs are computed from the **summed** counters of its sessions — never averaged from
the sessions' own KPIs. Averaging ratios weights a two-call session the same as a two-thousand-call
one, and the answer would not be the ratio of anything.

While a project is still counting, the card says `Counting n of m sessions — these totals are
partial`, and the chips are visibly partial until every session has landed.

---

## 4. Reading a row: the four states

The states are not cosmetic. They are the difference between "Pigeon has not counted this yet" and
"this session did nothing".

| State | What it means | What you see |
|---|---|---|
| **missing** | nothing has been requested for this row | "Metrics not loaded." |
| **pending** | the fold has not run yet | "Counting…" |
| **ready** | counted | the counters and the KPI chips |
| **unavailable** | the count could not be produced | the host's own sentence, plus **Try again** |

And within a ready row, a KPI itself has two states: a number, or `—` in a dimmed chip with
`not measured` for assistive technology. Rendering a null as zero would tell you your cache-rewrite
churn is perfect when in fact nothing was measured at all — the failure mode that makes a dashboard
worse than no dashboard.

Nullable counters follow the same discipline: `durationMs`, `reasoningTokens` and `providerCostUsd`
are **omitted** from the grid when absent rather than shown as zero.

### Why a row can sit at "Counting…"

`sessions_list` never folds a transcript. Claude's reach 14.6 MB and there are ~50 of them; folding
on the list path would cost seconds before the first row appeared. Rows arrive `Pending`, a
background task fills them, and each filled batch is emitted on `sessions://metrics`.

The cache key is the **source signature** — `(path, size, mtime)` for a file, `time_updated` for an
OpenCode row. Identical signatures must produce identical numbers, which is what makes a resume, a
console close, a cache eviction and an app restart all agree. A signature that has moved on means
the engine wrote more since we counted: the row goes back to *pending*, not *stale*.

Failures are cached too (ruling 5), so a transcript whose shape we cannot parse is not re-read on
every pass forever — and a file the engine later fixes by writing more gets a fresh attempt
automatically, because its signature moved.

---

## 5. Comparing numbers across engines

The counters are mapped into shared columns precisely so a project card can sum a Claude row and a
Codex row. Four things are worth holding in mind when you do.

1. **`apiCalls` does not mean the same physical event in all three.** Claude counts distinct
   `message.id`s; Codex counts `token_count` events; OpenCode counts assistant message rows. They
   are all "one model call" in their engine's own terms, but they are not the same instrument.
2. **`inputTokens` is uncached everywhere**, which required the Codex conversion in §2.2. That is
   what makes a summed project total meaningful at all.
3. **Reasoning tokens are reported by all three** — but Claude reports `None` where the engine
   stated nothing, while Codex and OpenCode always state a figure. A mixed project's reasoning
   total is honest; a Claude-only project's may legitimately be absent.
4. **Cost only ever comes from OpenCode.** A project total hides the cost figure entirely when no
   row states one, rather than showing `$0.00`.

The counters for Claude are additionally pinned to equal Demo Studio's for the same files
(FR-16, T-16.1) — the two products must agree on every number.

---

## 6. Two places where the code and the contract currently disagree

Both were found while writing this article. Neither is a fix; both are stated so the next person is
not surprised.

**OpenCode `apiCalls`.** `frds.md` §2.3.3 specifies
`COUNT(*) FROM part WHERE json_extract(data,'$.type')='step-finish'` — one step-finish part is one
model call, and the spec's cross-check (T-16.4) is that the step-finish tokens sum to the session
columns. The implementation at `src-tauri/src/adapters/opencode.rs:714` instead counts `message`
rows with `data.role == 'assistant'`. On the corpus recorded in `frds.md` Appendix A
(2026-09-12) those are **916 assistant messages against 873 step-finish parts** — not the same
number, so `contextPerCall` and `batchingRatio` for OpenCode are currently computed against a
denominator the contract does not specify. No decision record covers the change; it has been there since the adapter's first commit
(`d401ee7`).

**Codex token totals.** `frds.md` §2.2.5 specifies summing the per-event `last_token_usage` deltas.
`read_metrics` instead takes the last cumulative `total_token_usage`. The code documents why — the
two are cross-checked as equal in Studio, and the tail read is the cheaper of two equal answers —
and §2.2.5's own cross-check (T-16.3) asserts that equality. This one is a defensible
implementation choice that the contract has not caught up with, rather than a numeric divergence,
but it is not recorded as a ruling either.

There is also a genuinely **open** question, already flagged in `decisions/0001` and in
`read_metrics` itself: whether a Codex *resume* restarts the cumulative token counter. Zero of the
68 sessions on this Mac span two files, so there is no evidence either way. If one ever does and
the totals restart, "take the last event" silently under-counts, and that is the line that needs a
per-file last-value sum.
