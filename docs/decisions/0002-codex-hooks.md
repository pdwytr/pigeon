# 2. Codex waiting-on-you is read from Codex hooks, and the `~/.codex` write that installs them

Date: 2026-09-16

## The bug

A Codex sitting on an approval prompt was rendered **running**.

The rollout tail cannot see it. Codex's persisted records pair `task_started` with `task_complete`
or `turn_aborted` and nothing else; a session blocked on an approval still has an open
`task_started`. `docs/contracts` calls these owner diagnostics, and a diagnostic that says "working"
while the agent is actually waiting on you is the one lie this product must not tell — it is the
same failure class as an empty Live tab that looks like an idle machine.

Measured 2026-09-16 against `codex-cli 0.149.1` on this machine.

## What Codex actually offers

Three candidate sources were investigated and two were rejected on evidence:

- **`notify`** — receives exactly one event, `agent-turn-complete`, and never an approval. Not
  usable.
- **app-server thread status** — `thread/status/changed` does carry `active[waitingOnUserInput]`
  and `active[waitingOnApproval]`, which is exactly the right vocabulary. But status is
  **ownership-bound**: probed directly, a separate app-server reports every foreign thread as
  `notLoaded`. It would only ever cover sessions Pigeon itself owns, so a Codex the owner is
  driving in their own terminal would stay invisible.
- **hooks** — chosen. The `PermissionRequest` hook fires when Codex stops to ask, and it fires in
  every client that reads the user's config, terminal included. Verified end to end: an installed,
  trusted hook fired with no bypass flag and no credentials, and delivered
  `{"session_id", "hook_event_name", ...}` on stdin.

`request_user_input` — the model asking a question — has **no hook event**. `updatedInput`, the
field a reply would travel in, is reserved in Codex's own hook schema, and a hook that sends it
fails closed (openai/codex#28969). The question case therefore remains `running`, and this decision
does not pretend otherwise.

## The ruling

**Pigeon may drive Codex's own app-server to install a hook, and this is not a breach of the
read-only invariant.**

Invariant 1 forbids Pigeon writing, moving, locking, truncating or creating anything under
`~/.codex`. The install does write `~/.codex/config.toml` — but not by Pigeon. Every change goes
through Codex's `config/batchWrite`, the same call Codex's own TUI uses to record hook trust, so
**Codex parses, validates and writes its own file**. Pigeon opens `config.toml` for reading only,
and the byte-for-byte assertion in `tests/real_machine.rs` still holds.

The exception is bounded on purpose:

- **Consented.** One offer, in the hover, with `Yes` and `Not now`. `Not now` is remembered.
- **Reversible.** `codex_hooks_disable` removes the hook definitions and disables the trust state.
- **Trust is mandatory, not optional.** Codex silently skips an untrusted hook. An install that
  wrote the hook and stopped would report success and do nothing, so the install reads
  `hooks/list` back and only reports `trusted` when Codex says `trusted`.
- **Unchecked is not "no problem".** The status pass only ever *overrides* a `Running` reading, so a
  missing, unreadable or empty event file means "no hook evidence" — never "nothing is running".

## The mapping

- `PermissionRequest` → `NeedsYou`. The owner is being waited on.
- `PreToolUse`, `Stop` → no state of their own; the rollout tail keeps the running/idle call. The
  events exist only so a later one supersedes an approval that has been answered.
- A `PermissionRequest` may not override a turn the rollout has already closed. The event file is
  append-only and a line from an hour ago would otherwise resurrect a finished session forever.

## Consequences

- Codex approvals are honest across every client, including terminals Pigeon does not own.
- Codex questions are still `running`. Closing that needs upstream to open the reserved field; no
  amount of work in this repo can substitute for it.
- The install depends on an experimental Codex surface (`config/batchWrite`, `hooks/list`). The
  install fails loudly rather than half-succeeding, and `codex_hooks_status` tells the View whether
  the hook is there at all.
