# How Pigeon works

Design notes moved out of the README. Back to the [README](../README.md).

## Behavior

Almost all of this design lives in one distinction: **"we looked and nothing is there" is not
"we could not look."** The two are indistinguishable in a UI and mean opposite things, so every
rule below exists to keep them apart.

### Session status

What a row can show, and when.

| Status | Shown when | Where it comes from |
|---|---|---|
| `running` | an engine process is open and working | the engine's own status word |
| `delegating` | the parent is idle while one or more delegated child agents are still working | parent state + a live child transcript |
| `needs_you` | the engine is blocked on you — a prompt, a permission, or a question left outstanding | status word, or an unanswered tool call in the transcript tail |
| `finished` | the process is idle at its prompt, **or** there is no live process at all | a live `waiting` observation, or the absence of one |
| `unknown` | a process is present and the engine publishes nothing we can read | process table only |
| *no status* | the provider's status source could not be read this pass | a stated failure, not a guess |

`finished` deliberately covers two situations, because to an owner they are one: nothing is
happening and nothing is wanted. `unknown` is honest rather than lazy — an `unknown` that can be
explained is worth more than a `running` that was guessed. The last row is the important one:
it renders a neutral dash, never a badge.

### Four kinds of empty

These are four different answers and the View draws four different things. Collapsing any pair
is the one way this product can lie to you.

| Answer | Means | Rendered as |
|---|---|---|
| `null` status | the engine could not be read, so nothing is known | a neutral dash |
| no live observation | we looked, and nothing is running | `finished` |
| `unknown` | a process is there, the engine is silent | `unknown`, plus the raw word if it published one |
| `0` | we counted, and the answer really is zero | `0` |

The same rule governs numbers: a zero denominator is **undefined**, not zero, and a metric that
could not be produced is `unavailable` with its reason attached — never a `0` that looks counted.

### Counting

Live counts fold over live observations **only**; a closed session contributes to nothing. Every
live row lands in exactly one bucket, and a test asserts the buckets sum to the number of rows.

| Bucket | Fed by |
|---|---|
| `running` | `running` and `delegating` — delegation is a *kind* of running, not a fourth aggregate |
| `needs_you` | `needs_you` |
| `finished` | `waiting` — a live process idle at its prompt |
| `unknown` | `unknown` |

A session whose provider is degraded is in no bucket at all. It is not zero; it is unasked.

### Counters

Counters arrive in three states and none of them is zero: `pending` (not counted yet — the row
says "counting"), `ready`, and `unavailable` with the reason it failed. Listing never folds a
transcript; the largest on this machine is 14.6 MB and folding on the list path would cost
seconds before the first row appeared. Rows therefore arrive `pending`, a background pass fills
them, and each batch is emitted on `sessions://metrics`.

A fold already running for a session is never started a second time — a caller that arrives
mid-fold stays `pending` and takes the value from that event. Counts are cached by source
signature, so a resume, a console close, a cache eviction and a restart all agree.

**No invented dollar cost.** Where an engine states its own figure it is shown, labeled as the
engine's number. Where an engine publishes no allowance, no bar is drawn — an unsupported
allowance is an absence, not a zero.

### Delegation

A parent agent sitting idle while its subagents work is the case that used to read as `finished`,
which is exactly backwards: it is the session you should *not* interrupt. A parent that is idle
with at least one active child reads `delegating`, and the evidence names how many.

A child only counts as active if its transcript has been written to within **15 minutes**. A
crashed or quota-stopped child can sit on an unfinished tool call forever, and without the window
a historical sidecar would pin its parent at `delegating` indefinitely. The window is long enough
for a slow tool and short enough that a dead child stops lying.

### What each engine publishes

Status degrades **per provider**. One unreadable root removes that engine's sessions from the
live set and nothing else — the other two are unaffected, and the sessions that dropped out are
marked with no status rather than `finished`.

| Engine | How a process is tied to a session | If that is unavailable |
|---|---|---|
| **Claude Code** | `~/.claude/sessions/<pid>.json` names its own `sessionId` outright | no file, no observation — the session is simply not in the live set |
| **Codex** | a live process holds `~/.codex/thread-writer-locks/<thread>.lock`, and the filename *is* the thread id | no lock, no attribution |
| **OpenCode** | an installed bridge records which session each pid is working on | falls back to the working directory, and **refuses** when two sessions share one |

The OpenCode fallback is the one place a guess is possible, so it is the one place a guess is
explicitly declined: a cwd is a folder, two sessions can share it, and attributing both to the
newest hid a real session on this machine. Ambiguity attributes nothing.

### When something cannot be read

Every scope carries its problems. A list, a project rollup and the status snapshot each return
the failures that shaped them, so an empty Live tab can always say why it is empty.

One class is filtered: a **permanent design limit** is not a failure and is not reported as one.
An engine that publishes nothing of some kind by design would otherwise raise the same banner on
every poll, which trains you to ignore the banner that matters. Today's failures are shown;
known limits are not.

### Stopping a session

Only processes that can be **proven** to belong to the session are stopped. If two processes
could be it, that is an ambiguity, it is reported as one, and **nothing is stopped** — there is
no "probably right" kill. Stopping is idempotent: a session that has already exited reports
success with nothing stopped, not an error.

### What never leaves the machine

Nothing is sent anywhere. Beyond that, two things never enter a payload, a log, an error or a
`Debug` line: **credentials**, and **command lines**. Argv can hold a prompt, so the process probe
asks for `pid,ppid,comm` and never for `args` — the cheapest way to guarantee a prompt cannot
leak into an evidence string is to never read one.

## The rules this codebase keeps

Inherited from Demo Studio, where each was learned the expensive way.

1. **Read-only on sources.** Snapshot and report; never write back.
2. **Fail loud on unknown shapes.** Engine formats drift — twice, provably. An adapter that meets
   a record it does not recognise says so. It never renders a number derived from a guessed schema.
3. **The counting rules are frozen.** Dedupe API calls by `message.id`; within one id take the
   per-key elementwise **MAX**, because `output_tokens` is a streaming counter. Summing raw lines
   double-counts 2–6×.
4. **These numbers are owner diagnostics, never agent targets.** Handing a builder a Studio metric
   to hit is how you get Goodhart's law instead of a measurement.
5. **Hold a lock only for the work that needs it.** Resolve, lock, write, unlock. Never across a
   spawn, a PTY write, a `wait`, an emit, or a filesystem walk.
6. **A session is `(provider, sid)`.** Never `sid` alone — two engines may mint the same uuid — and
   never a prefix: UUIDv7 prefixes collide every ~65 seconds.
7. **Null, pending, unavailable and zero are four different things**, and the View renders four
   different things.

## Not in this pass

macOS signing and notarization, Windows and Linux runtime verification, multi-profile logins,
filesystem watching, and any persistent store.
