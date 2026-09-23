# 4. OpenCode waiting-on-you is read from a plugin bridge Pigeon installs

Date: 2026-09-18 · **Status:** accepted

## The bugs

**One.** An OpenCode sitting on a permission prompt was rendered **running**.

**Two.** Two live `opencode` processes in one folder were shown as **one** session, and the second
session was not observed at all. Measured on this Mac 2026-09-18: pids 7587 and 66939, both
`cwd=/Users/khalid/Documents/Projects/pigeon`, both resolving to the same newest session, which
`collapse` then merged into a single row carrying both pids.

The database cannot see either. Measured against `opencode` 1.18.31:

- the `permission` table held **zero** rows during a real `external_directory` ask that held the
  turn for the whole wait. It is the *saved rules* table (`/api/permission/saved`) — a row is a
  standing "always allow", not a pending ask. The adapter had been reading it as a pending ask,
  which was a latent false-positive: any saved rule would pin every session in that project at
  **needs you** forever;
- the `event` stream carried **no** `permission` part. The tool part went `pending → running` and
  sat there, indistinguishable from a tool that is genuinely executing;
- a live process holds **no per-session file, socket or port**. Both processes held only the shared
  `opencode.db`, `-wal`, `-shm` and the log, and neither listened on any port;
- the only trace of an ask was a line in `log/opencode.log` (`message=asking id=per_...`), which
  names a `run` id and **not** a session id — and one run can own two sessions (a parent and its
  subagent), so attributing an ask from the log would be a guess.

A process→session link therefore did not exist on disk at all, so attribution had to fall back to
"the newest session in the cwd" — a **folder**, which the code's own comment already admitted two
sessions can share.

## What OpenCode actually offers

Both facts **are** visible, in-process: OpenCode's bus emits `permission.asked` /
`permission.replied` with `properties.sessionID`, and the same events carry the session a process is
working on (read out of the 1.18.31 binary; the same events the TUI's own notification handler
consumes). A plugin runs *inside* the process, so `process.pid` is its own — which is exactly the
link the filesystem lacks. A plugin's generic `event` hook observes all of it. The typed
`permission.ask` hook is **dead** — declared in `@opencode-ai/plugin` but never triggered
(openai/opencode #7006, #19469, #9229) — so the generic hook is the only route.

## The ruling

**Pigeon may install a plugin into the owner's OpenCode config, and this is not a breach of the
read-only invariant.**

Invariant 1 forbids writing under `~/.claude`, `~/.codex` and `~/.local/share/opencode`. The plugin
is written to `~/.config/opencode/plugins/` — the owner's *config* directory, which OpenCode does
not treat as data. Pigeon still never writes `~/.local/share/opencode`, and
`tests/real_machine.rs`'s byte-for-byte assertion is unaffected. The exception is bounded exactly
as the Codex hooks exception is (`0002-codex-hooks.md`):

- **Consented.** One offer, in the hover, with `Yes` and `Not now`. `Not now` is remembered, under
  a key of its own so dismissing Codex's offer does not dismiss OpenCode's.
- **Reversible.** `opencode_hooks_disable` removes the file. Removing what is not there is not an
  error.
- **Identified by a marker, not a filename.** `is_installed` reads the file back and looks for
  Pigeon's marker, so a foreign plugin of the same name is not mistaken for ours.
- **Unchecked is not "no problem".** The status pass only ever *overrides* a running reading, so a
  missing, unreadable or empty event file means "no bridge evidence" — never "nothing is running".

## The mapping

- `permission.asked` for a session → **needs you**; `permission.replied` clears it. The bridge
  tracks **request ids**, not "the last event wins": two asks can be outstanding at once and a reply
  for one must not clear the other. An ask with no request id is dropped rather than tracked as a
  bare session, because a reply could never clear it.
- A session claim (`kind: "session"`) records that this pid is working on this session. The newest
  claim for a pid wins, and a claim for a session discovery did not mint as a **root** (a subagent,
  an archived row) is skipped in favour of the next-newest, so the process resolves to the root the
  owner is driving.
- **Every record carries the writing process's pid and start time.** macOS reuses pids, so a claim
  from an earlier process start is ignored and a later one resets what was there; a new `opencode`
  that inherited a dead one's pid is never shown as the dead one's session.
- **Attribution is exact when the bridge can speak, and refused when it cannot.** With no claim for
  a pid, the folder is the fallback — but only when exactly one discovered session shares it. Two
  sessions in one folder is a coin flip, and this file does not flip coins (invariant 10).
- The plugin never replies, never blocks, and swallows its own errors: a bridge that breaks OpenCode
  is worse than one that misses an event.
- **A restart is needed** for the plugin to load, and the install message says so.

## Consequences

- OpenCode permissions are honest, and the saved-rules false-positive is gone: `pending_permission`
  was removed, not just demoted.
- Two live OpenCode processes in one folder are two rows, each on its own session — proven against
  the real database (31 root sessions; pid 7587 → its session, pid 66939 → the other).
- Without the install, the second bug is fixed the *honest* way: the folder fallback refuses to
  attribute when more than one session shares it, so the ambiguous sessions are simply not observed
  rather than collapsed into one wrong row. This is a visible regression in row count on an
  unbridged machine, and it is the intended trade — a missing row is recoverable, a wrong one is a
  lie.
- The plugin is JavaScript loaded by OpenCode's Bun runtime, so it is not covered by Pigeon's own
  test suite beyond the source-shape assertions. The reader, the claim/pid-reuse logic, and the
  state mapping are covered, and the install is exercised against a temp config dir.
