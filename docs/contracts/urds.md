# Pigeon — User Requirements Document (URD)

**Status:** draft v0.1 for owner review · **Date:** 2026-09-12 · **Owner:** the owner (this machine's user) ·
**Author:** Claude Fable 5.1, from the owner's statements of 2026-09-12 · **Companion:** `frds.md`
(functional requirements, interface contracts, verification), which cites this document's `UR-##` ids.

Every requirement below carries an **Origin** in the convention the owner already uses in
demo-studio's `docs/LORE.md`: **owner** (the owner's statement or ruling), **agent** (a finding or
proposal from agentic work), **joint** (owner-directed investigation with agent findings, or an
agent proposal the owner shaped and ratified). Ids are never renumbered or reused. A requirement's
**Priority** is `Must` (the first pass does not ship without it), `Should` (the first pass ships
without it only with the owner's say-so) or `Could` (wanted, not scheduled).

---

## 1. Why this exists

demo-studio started as a flight recorder for agentic builds and grew into a full observability
product: a vault that mirrors every transcript on the machine, a SQLite store with seventeen schema
versions, a capture pipeline, a derive layer with twelve frozen KPIs, a Python sidecar, and a dozen
panels. It is the right product for the studio's mission. It is the wrong size for a second, simpler
need the owner stated on 2026-09-12:

> "I wanna create a new repo and do a pigeon version, a bare bones version of this project. Stack
> should be same rust, typescript, python, react. I just want one view for it. And I want it to show
> me account visibility, ability to look at sessions and ability to open/resume them in terminal
> inside the app with a click."

The owner later dropped Python from the stack ("Drop Python"), added a fourth engine-agnostic need
("I want support for [OpenCode] right off the bat ... this is the real moat, xAI and Facebook
eventually"), added per-session and per-project cost-and-usefulness numbers ("give the user a feel
of cost, usefulness of the sessions in a succinct way"), and added live status ("I want a way to
know running agents, completed ones and ones waiting on my response ... in the top hover and also a
way to see it in the app").

**Pigeon** is that product: one window, three engines today, no store, no vault, no sidecar, and
nothing on screen the owner did not ask for. It is a sibling of demo-studio, not a fork: it
copies the studio's proven code where the studio already solved a problem (the PTY runtime, the
terminal surface, the credential readers, the counting rules) and never imports it.

## 2. Who uses it, and where

- **One user, the owner**, on the owner's own machines. No multi-user, no sharing, no hosting.
- **Primary machine for the first pass: this Mac** (Apple silicon, macOS 15). The owner's Windows
  box is the second target; the code must keep compiling there, but nothing is verified on it in the
  first pass. Wherever this document says **"this machine"** it means the machine the fact was
  measured on, and names it — the studio's rule, kept.
- **Three coding agents installed and in daily use**, verified on this Mac on 2026-09-12:

  | Engine | Version | Binary | Login type |
  |---|---|---|---|
  | Claude Code | 2.1.269 | `~/.local/bin/claude` | Claude subscription (OAuth) |
  | Codex CLI | 0.149.1 | `~/.bun/bin/codex` | ChatGPT subscription (plan `team`) |
  | OpenCode | 1.18.29 | `/opt/homebrew/bin/opencode` | provider API keys (`opencode`, `opencode-go`) |

  The owner also runs Codex through the VS Code extension (three `codex app-server` processes were
  alive at measurement time) and keeps long-lived interactive sessions open for days (one Claude
  Code session had been alive for 18 days).
- **Usage pattern.** Several sessions run concurrently across several projects. The owner switches
  between them, resumes old ones, and wants to know before starting work whether there is room left
  in the allowance and which sessions need a reply.

## 3. Goals and non-goals

**Goals (the first pass).**
1. See, in one window, who is signed in to each engine and how much allowance is left.
2. See every session from every engine on this machine, newest first, with enough to recognise it.
3. Resume any of them inside the app with one click, in a real terminal, in the right directory.
4. Know at a glance what is running, what is waiting on the owner, and what has finished — from an
   always-on-top hover and from inside the app.
5. See what a session and a project cost and produced, in six counters and three KPIs, with the
   same numbers the studio would show.
6. Run from a fresh clone with no setup, and never touch the engines' files except to read them.

**Non-goals (deliberate).** No transcript viewer, no search, no history or trends, no vault or
mirror, no store, no KPI persistence, no stage taxonomy, no lineage or subagent tree, no
markdown viewer, no multi-profile login management, no agentic control of the app, no hosting,
no telemetry to anyone. Anything the owner did not name is out, even when the studio has it.

## 4. Operating principles the owner already rules by

These are the studio's invariants restated for a product with no store. They bind every requirement
below and are repeated in the FRD as non-functional requirements.

- **P1 Read-only on sources.** Pigeon never writes, moves, renames, locks for writing, or truncates
  a file another engine owns. It reads transcripts, rollouts, databases, config and credential files
  in place, and nothing else.
- **P2 Fail loud on unknown shapes.** When an engine's file, record or endpoint has a shape pigeon
  does not recognise, the screen says so in words. Pigeon never renders a number derived from a
  guessed schema, and never renders zero where the truth is "unknown".
- **P3 Counting rules are frozen and shared with the studio.** Token and call counts follow the
  studio's `derive/` definitions exactly; a change to a definition needs an ADR in both repos.
- **P4 KPIs are owner diagnostics, never agent targets.** No pigeon number is ever placed in a brief
  as a target for an agent to hit.
- **P5 UTC inside, local time at the edge.** Every stored or computed timestamp is UTC; only the
  screen converts to local time.
- **P6 Privacy.** Local-only. Credentials never reach a log, an error message, the screen or any
  network destination other than the engine vendor's own endpoint that the engine itself uses.
- **P7 The owner's CLIs, never bundled.** Pigeon launches the `claude`, `codex` and `opencode` the
  owner installed, found on the owner's PATH at launch time. It never ships, vendors or updates them.
- **P8 Engines are adapters.** Adding an engine (xAI, Meta, or whoever is next) touches one adapter
  and no other layer. Removing one leaves the rest unchanged.
- **P9 Pigeon owns process control.** Pigeon may terminate engine processes only through a
  session-specific stop operation that proves the process belongs to the selected session. Stop
  applies to Pigeon-launched and externally-launched matching processes; it never kills an unrelated
  process. Source files remain read-only.

## 5. User requirements

### UR-1 One window, one view
**Statement.** Everything Pigeon offers is visible in a single window without navigation: an
account strip across the top, a project-first workspace on the left, and a right-hand pane that holds
the selected project or session detail and, below it, an embedded terminal.
**Rationale.** "I just want one view for it." The studio's value is depth; pigeon's is glance.
**Origin.** owner · **Priority.** Must
**Acceptance.**
- A1. On launch the window shows the account strip, the project workspace and an empty right pane with no
  further clicks.
- A2. No menus, drawers, terminal tabs or secondary windows are required to reach any feature in this
  document, except the hover (UR-8), which is a second, always-on-top window by design.
- A3. The window works at 1200 × 720 and up; the list and the right pane are resizable against each
  other.

### UR-2 Account visibility: who is signed in, and how much is left
**Statement.** For each installed engine the owner sees the signed-in identity (e-mail or display
name, organisation, plan words as the engine states them) and, where the engine exposes it, how much
of the **5-hour** and **weekly** allowance is used, with the time each window resets.
**Rationale.** "Is there room to start this?" is the question that comes before a run; the studio
answered it on the Overview and the owner wants it kept.
**Origin.** owner · **Priority.** Must
**Acceptance.**
- A1. Claude Code: identity from the CLI's own config; two bars (5-hour, weekly) from the same
  endpoint the CLI's `/usage` panel uses; reset times in local time.
- A2. Codex: identity (login mode, account id) from the CLI's own auth file; two bars (5-hour,
  weekly) from the CLI's own rate-limit statement in the newest rollout; a **stale** marker when
  that statement is older than one hour; the engine's own words when a limit has been reached.
- A3. OpenCode: identity = the providers signed in (names and kind, never keys) and the account
  e-mail if the engine has one; capacity shows "not exposed by this engine", never a bar at zero.
- A4. Readings refresh every 15 minutes and on demand; the strip shows when each reading was taken.
- A5. When a reading cannot be made, the strip states the reason in one sentence (not signed in,
  endpoint refused the login, shape changed, offline) and shows no bar for that window.
- A6. Nothing in the strip, a log, or an error message ever contains a token, key or header value.

### UR-3 Project-first Live and Recent workspace
**Statement.** The owner sees projects first, with their sessions available in an expandable list.
The first view is **Live** and shows only projects containing a live session plus those live sessions.
The second view is **Recent** and shows only projects with at least one session active in the last
seven days, plus only those seven-day sessions. Historical projects and sessions are not shown in
Recent.
**Rationale.** The owner wants current work at a glance, not an archive. Projects are the primary
unit of work; sessions are the activity inside a project.
**Origin.** owner; the engine facts are agent findings · **Priority.** Must
**Acceptance.**
- A1. Live is the selected first view on launch.
- A2. Live shows a project when at least one session has live process evidence; expanding a project
  shows its live sessions only.
- A3. Recent shows projects with a session whose last activity is no older than seven days; expanding
  a project shows only sessions inside that same seven-day window.
- A4. Projects are sorted by newest qualifying session; sessions inside a project are sorted by last
  activity descending, ties by engine then id.
- A5. A project card shows its leaf, normalized path, live/session counts, summary metrics, and actions
  to open the folder or start a session in Claude, Codex, or OpenCode.
- A6. Each session row shows engine badge, title, last-active time, status when present, and metric
  chips once available (UR-5).
- A7. A Codex session resumed many times is one row, not one per rollout file; a Codex subagent
  thread is not a row. An OpenCode child session is not a row. A Claude Code subagent transcript is
  not a row.
- A8. An engine whose files are absent (not installed, or never used) contributes no rows and one
  quiet chip in the account strip saying so; the other engines' rows are unaffected.
- A9. A project with no qualifying session is absent from the selected view; it is not shown as an
  empty project placeholder.

### UR-4 Resume or start a session in the embedded terminal
**Statement.** Clicking **Resume** on a row opens the session inside the app in a real terminal
running the engine's own resume command, in the session's own working directory. From a project card,
the owner can start a new session in Claude Code, Codex, or OpenCode in that project directory. The
terminal appears in the existing right pane below the selected detail; there are no visible terminal
tabs. The owner types into it exactly as in Terminal.app.
**Rationale.** "Ability to open/resume them in terminal inside the app with a click." Studio proved
the runtime on Windows; the owner wants it on the Mac.
**Origin.** owner · **Priority.** Must
**Acceptance.**
- A1. Claude Code rows run `claude --resume <session-id>`; Codex rows run `codex resume <uuid>`;
  OpenCode rows run `opencode --session <id>`; each in the session's directory.
- A2. The engine's banner is visible in the pane within about two seconds of the click on this Mac.
- A3. Keystrokes, paste, arrow keys, Ctrl-C, colours and cursor movement behave as in Terminal.app;
  resizing the pane resizes the terminal.
- A4. Closing the embedded terminal ends the process; the process ending shows an exit notice with its
  code in the pane until the owner closes it.
- A5. A row whose directory no longer exists offers no Resume and says why on hover.
- A6. A Resume click on a session that is already live in Pigeon focuses its existing terminal instead of
  starting a second process.
- A7. Add session offers exactly three engine choices: Claude, Codex, and OpenCode.
- A8. New project opens a native folder picker; choosing a folder makes it the project context for a
  new session without writing a Pigeon project record.
- A9. The same behaviour holds when Pigeon is opened from Finder or the Dock, not only from a
  terminal (the GUI launch has no PATH — see FRD FR-28).

### UR-5 Cost and usefulness at a glance, per session
**Statement.** Every session shows six counters — **input tokens, output tokens, cache read, cache
write, API calls, tool calls** — and three KPIs — **context per call, rewrite ratio, batching
ratio** — computed exactly as demo-studio computes them. The detail pane adds user turns and
wall-clock duration as the usefulness cues.
**Rationale.** "My idea is to give the user a feel of cost, usefulness of the sessions in a succinct
way." Tokens are the honest cost signal for subscription logins; the three KPIs are the studio's
frozen ones that need no stage classification.
**Origin.** owner (the ask), joint (the choice of three) · **Priority.** Must
**Acceptance.**
- A1. Rows show compact chips (e.g. `1.2M tok · 84 calls · 61 tools`); the detail pane shows all six
  counters spelled out and the three KPIs with one-line meanings.
- A2. For three Claude Code sessions present in both products, pigeon's six counters equal the
  studio's session-detail figures exactly.
- A3. A KPI whose denominator is zero is shown as absent ("—"), never as 0.
- A4. Chips appear progressively: the list paints first, chips fill in as sessions are counted, and a
  session not yet counted shows a pending chip, not a blank.
- A5. OpenCode's own dollar figure is shown on OpenCode sessions labeled as the engine's number. No
  dollar figure is invented for any engine.

### UR-6 Project-first rollups and actions
**Statement.** Every qualifying project is a primary item in Live or Recent. Expanding it shows its
sessions; selecting it shows the project's six counters summed across engines, the three KPIs
recomputed from those sums, session counts by engine, and counting coverage.
**Rationale.** "I wanna see the KPIs and other metrics at a project level too." A folder is where
work happens; the owner wants to know what a project consumed regardless of which engine did it.
**Origin.** owner · **Priority.** Must
**Acceptance.**
- A1. A project card shows the leaf name, path, engine badges with counts, summed chips, status counts,
  and the three KPIs; selecting it opens the project detail in the right pane.
- A2. Sums equal the sum of the visible rows' counters; KPIs equal the formulas applied to the sums,
  not the average of row KPIs.
- A3. The card states `counted n of m sessions` while counting is in progress and the chips are
  visibly partial until `n = m`.
- A4. A directory spelled with and without a trailing slash, or (on Windows) in a different case, is
  one project.
- A5. The project card's Open folder action opens the selected directory in the native file manager;
  Add session offers Claude, Codex, and OpenCode.

### UR-7 Always know what is running, what is waiting on me, and what is done
**Statement.** Every session with a live process is in exactly one of three states, decided from
the engine's own evidence, and the owner can see the state everywhere a session is shown:
- **running** — the agent is working (a turn is in flight);
- **needs you** — the agent is blocked on the owner: a permission prompt or a question it cannot
  proceed without (Claude Code's own word for this is `needs_input`);
- **finished** — the agent answered and is waiting for the owner's next instruction; the process is
  alive and idle at its prompt.
A session whose process is gone has **no status**: no badge, no count. It is a row in the list with
a last-active time, and Resume is how it comes back. Owner ruling 2026-09-12: "no point in counting
done ones, there could be a lot of them, user won't care."
"Waiting on my response" in the owner's words is **needs you** and **finished** together; the two
are shown distinctly because the first is urgent and the second is not. A fourth badge, **unknown**,
appears only when a process is alive but the evidence for its state is missing, with the missing
signal named. Counts of running, needs you and finished are always one glance away.
**Rationale.** "I want a way to know running agents, completed ones and ones waiting on my
response." With several concurrent sessions, the expensive failure is an agent sitting on a
permission prompt for an hour while the owner works elsewhere. The studio can only infer liveness
from a transcript's tail and ages every open observation out after 20 minutes; it reads no process
facts. Pigeon adds the engines' own status files and the process table, so "finished" and "no
process" become observations rather than timeouts. The owner's earlier studio ruling stands — an
idle session is not *lit as active* — which is why **finished** is its own state and never counted
as running.
**Origin.** owner (the need); agent (the feasibility evidence below) · **Priority.** Must
**Feasibility, verified on this Mac 2026-09-12.**
- Claude Code writes `~/.claude/sessions/<pid>.json` for every running CLI, carrying `sessionId`,
  `cwd`, `name`, `kind`, `status` (observed values: `busy`, `idle`), `updatedAt`, `statusUpdatedAt`.
  Four were live at measurement (one `busy`, three `idle`). A file whose pid is dead marks a
  finished session. The CLI also offers `claude agents --json`, which prints the same live set
  (`pid`, `cwd`, `kind`, `name`, `sessionId`, `startedAt`, `status`) in ~180 ms and whose
  documentation names `needs_input` as "blocked and waiting for your response"; `--all` adds
  completed background sessions. Pigeon reads the files (richer: `statusUpdatedAt` gives
  time-in-state) and uses the command as the cross-check in tests.
- Claude Code also writes an `ai-title` record into the transcript (`{"type":"ai-title","aiTitle":
  "Pigeon version new repo"}`) — the CLI's own generated title, which pigeon prefers over the first
  prompt when present.
- A live Codex process holds its rollout file and `~/.codex/thread-writer-locks/<thread>.lock` open
  (seen with `lsof`), which maps the process to its session exactly; the rollout's last event
  (`task_started` / `task_complete` / `turn_aborted`) says whether a turn is in flight.
- A live OpenCode process exposes its working directory; its database marks an assistant message
  complete or not. Its `permission` table holds *saved rules*, not pending asks; a pending ask is
  visible only in-process, so Pigeon reads it from an installed plugin bridge (FR-21b).
**Acceptance.**
- A1. The state of a Claude Code session changes on screen within 5 seconds of the CLI changing it.
- A2. A session the owner resumes from pigeon shows **running**/**waiting** from its own console's
  liveness immediately, without waiting for the file signals.
- A3. When a process is alive but its state cannot be decided from evidence, the badge says
  **unknown** and the hover text says which signal was missing. Pigeon never drops the badge of a
  session whose process is alive.
- A4. A Claude Code `status` word pigeon has not seen before is shown verbatim as the badge text,
  not mapped to a guess.

### UR-8 The hover: an always-on-top glance at live sessions
**Statement.** A small, frameless, always-on-top window — the studio's "side hover" idea, kept —
shows the counts of **running**, **needs you** and **finished** sessions and lists them (engine,
project, name, state, how long in that state), updating live. Sessions with no live process never
appear in it. It stays visible over other apps, can be
dragged to any screen corner, remembers its position, and clicking a listed session brings pigeon
forward with that session selected.
**Rationale.** "I love the side hover studio has which shows running agents ... I want this detail in
the top hover." The owner works in other windows while agents run; the hover is how they know to
come back.
**Origin.** owner · **Priority.** Must
**Acceptance.**
- A1. The hover can be shown/hidden from the main window and remembers its last position and
  visibility across launches.
- A2. It shows three counters and a list of at most eight sessions (needs you first, then running,
  then finished), each with engine badge, project leaf, session name or title, state, and
  time-in-state.
- A3. A session that moves to **waiting on you** rises to the top and is visually distinct within 5
  seconds of the change.
- A4. Clicking a row brings the main window to the front with that session selected; clicking the
  counter area alone only brings pigeon forward.
- A5. It occupies no more than roughly 320 × 240 points and never steals keyboard focus. (A
  click-through "ghost" mode is a Could: Tauri offers whole-window cursor pass-through on macOS but
  not per-region, so it would be a toggle, not a default.)

### UR-9 Live view and status inside the app
**Statement.** Inside the main window the owner can select **Live** to see only live projects and
their live sessions. A status badge appears wherever a live session appears, with the state and
time-in-state on the detail pane. The status rail has the same counts as the hover.
**Rationale.** "and also a way to see it in the app."
**Origin.** owner · **Priority.** Must
**Acceptance.**
- A1. Badges: running · needs you · finished · unknown, each with a distinct colour and a text
  label (never colour alone). A session with no live process has no badge.
- A2. Live shows only projects with at least one live session and only those live sessions.
- A3. Recent shows only projects and sessions active in the last seven days.
- A4. The three counters in the status rail and in the hover always agree, because they are the
  same data.

### UR-10 Zero setup, no store, no sidecar
**Statement.** A fresh clone runs with `npm install` and `npm run tauri dev`; a built app runs from
a double-click. Pigeon keeps no database, no vault, no mirrored copy of anything and no state
beyond window positions and the hover's visibility. Everything it shows is read from the engines'
own files when asked, and cached only in memory.
**Rationale.** The studio's setup ceremony (test vault, ports, venvs, migrations) is the cost the
owner is escaping. With no store there is nothing to migrate and nothing to protect.
**Origin.** owner ("Drop Python", "bare bones"); agent (the no-store shape) · **Priority.** Must
**Acceptance.**
- A1. No Python, no sidecar process, no SQLite file pigeon owns.
- A2. Killing pigeon at any moment loses nothing but the caches; the next launch shows the same
  data within the refresh cadences.
- A3. The only files pigeon writes are its own log and a small settings file in its own app-data
  folder.

### UR-11 Read-only on the engines' files
**Statement.** Pigeon opens engine files read-only and never takes a write lock on them.
**Rationale.** Principle P1; an engine's transcript is the engine's, and a reader that blocks a
writer would corrupt the very thing being observed.
**Origin.** owner (studio invariant 1) · **Priority.** Must
**Acceptance.**
- A1. Reading OpenCode's live database never blocks OpenCode writing to it (WAL read-only open).
- A2. No engine file's modification time changes because pigeon read it.
- A3. A file that appears mid-write (truncated last line) is skipped for that line, not treated as
  corrupt.

### UR-12 Privacy and credentials
**Statement.** Pigeon is local-only. The only network call it ever makes is the Claude usage
endpoint the CLI itself calls, with the CLI's own token, which never leaves the function that reads
it. Nothing pigeon logs, displays or serialises contains a token, key, cookie or authorization
header.
**Rationale.** Principle P6; the studio's ADR-0043 rules, kept.
**Origin.** owner · **Priority.** Must
**Acceptance.**
- A1. A test with a sentinel token in the credentials fixture proves the sentinel is absent from
  every error, every serialised payload and the log, across every error class.
- A2. OpenCode's `account` table is read for its `email` column only; token columns are never
  selected.

### UR-13 Honest numbers
**Statement.** Pigeon never shows a number it cannot justify from the engine's own record. Unknown
shapes, renamed fields, missing buckets and undocumented endpoints produce a stated absence.
Counting rules are the studio's, verbatim.
**Rationale.** Principles P2 and P3. A weekly bar drawn full because a field went missing is the
worst lie this surface could tell (the studio's words).
**Origin.** owner · **Priority.** Must
**Acceptance.**
- A1. A Claude usage body missing `five_hour` shows no 5-hour bar and the sentence "the usage
  endpoint's shape has changed".
- A2. A Codex rollout whose rate-limit slot is not an object shows no bar and says so.
- A3. Pigeon's Claude counters equal the studio's for the same files (UR-5 A2).

### UR-14 Mac-first, launchable from Finder, compiling everywhere
**Statement.** The first pass is verified on this Mac, including launch from Finder and the Dock;
the same source compiles for Windows and Linux without platform forks in the View.
**Rationale.** The owner's daily machine is now a Mac; the Windows box follows.
**Origin.** owner · **Priority.** Must (Mac), Should (compile matrix)
**Acceptance.**
- A1. The manual smoke test in FRD §10 passes from `tauri dev` and from a packaged `.app` opened in
  Finder.
- A2. `cargo check --all-targets` passes for macOS, Windows and Linux targets in CI.

### UR-15 Engines are adapters; OpenCode is in from day one
**Statement.** Claude Code, Codex and OpenCode are supported in the first pass, each behind the same
adapter contract, so a fourth engine is one new adapter and nothing else changes.
**Rationale.** "Don't make OpenCode a step improvement, I want support for it right off the bat ...
this is the real moat, xAI and Facebook eventually."
**Origin.** owner · **Priority.** Must
**Acceptance.**
- A1. The View has no engine-specific branches; it renders whatever the adapters report.
- A2. Removing an adapter at compile time leaves every other engine's rows, identity, capacity,
  status and metrics unchanged (the studio's adapter acceptance test, kept).
- A3. OpenCode sessions, identity, status and metrics are present in the first demonstrable build.

### UR-16 KPIs are diagnostics, never targets
**Statement.** No pigeon number is ever presented as a goal for an agent, exported for a brief, or
worded as a score.
**Rationale.** Principle P4; the Goodhart lesson from the harness.
**Origin.** owner · **Priority.** Must
**Acceptance.** No "score", "grade" or ranking appears anywhere in the UI or the docs.

### UR-17 What the owner does not want to see
**Statement.** Pigeon does not show which model a session is configured to use; it does not
estimate dollars for subscription engines; it shows no chart, sparkline or trend; it does not show
the transcript text.
**Rationale.** "I don't want to know what model the session is currently configured to." Every
element on the one view earns its place by an owner request.
**Origin.** owner · **Priority.** Must
**Acceptance.** None of the four appear.

### UR-18 Stop sessions and resume them later
**Statement.** The owner can stop a session from Pigeon. Stop terminates every engine process that
Pigeon can prove belongs to that session, including a process originally opened outside Pigeon. A
stopped session remains in the source history and can be resumed later. Pigeon does not silently
stop an ambiguous process match.
**Rationale.** Pigeon is the control surface for the owner's agent work, not only an observer. The
same session must not continue running in another terminal after the owner stops it here.
**Origin.** owner · **Priority.** Must
**Acceptance.**
- A1. A live or finished process-backed session has a `Stop session` action; a session with no matching
  process has no destructive action and states that it is already stopped.
- A2. Stop uses engine-specific process/session evidence, sends graceful termination first, then force
  termination after the bounded timeout, and reports which processes were stopped.
- A3. If Pigeon cannot prove ownership of a process, it refuses to kill it and shows an explicit
  ambiguity error rather than guessing.
- A4. After stop, every matching process disappears from the Live view and any Pigeon console shows
  an exit notice. The source transcript remains untouched.
- A5. Resume starts the engine's normal resume command again and does not create a duplicate while a
  matching process is still alive.
- A6. A Pigeon-owned terminal remains alive when the owner selects another session. Returning to the
  session reattaches to the same console and replays its bounded scrollback before live bytes.
- A7. Closing or quitting Pigeon terminates every Pigeon-owned console process; engine sessions and
  source files remain intact.

### UR-19 Configurable polling and Recent window
**Statement.** The owner can configure how often Pigeon polls engine sources and how much recent
activity the Recent view includes. Defaults are a five-second poll interval and a seven-day Recent
window.
**Rationale.** Engine files change continuously, but the owner should control the freshness/CPU
tradeoff and the amount of working history visible in the primary surface.
**Origin.** owner · **Priority.** Must
**Acceptance.**
- A1. `pollIntervalSeconds` is persisted in Pigeon settings, defaults to `5`, and applies to source
  refresh scheduling without writing on every poll tick.
- A2. `recentWindowDays` is persisted in Pigeon settings, defaults to `7`, and determines the Recent
  cutoff as `now - recentWindowDays`.
- A3. Values are validated to safe bounds: poll interval `1..300` seconds and Recent window `1..365`
  days; invalid or corrupt settings revert to defaults.
- A4. Changing either setting takes effect on the next refresh and immediately re-queries the selected
  scope; it does not rewrite engine files or create a Pigeon history.

## 6. Scenarios

**UC-1 First launch.** The owner double-clicks the app. Within a second the window shows: the
account strip with three cards (Claude Code: e-mail, org, two bars; Codex: plan and two bars with a
"reading from 09:10" note; OpenCode: two providers, "capacity not exposed"), the **Live** project
workspace first, live projects expanded to their live sessions, and the **Recent** option available
for projects and sessions active in the last seven days.

**UC-2 Room to start?** Before dispatching a long job the owner glances at the strip: Claude 5-hour
bar at 76 %, resets 14:30; weekly at 31 %. Codex 5-hour 35 %, weekly 6 %, plan team. They start.

**UC-3 Pick up where they left off.** The owner opens Recent, expands the
`demo-insights` project, clicks yesterday's Codex session titled "Add the atlas
legend", and clicks **Resume**. The Codex banner appears in the existing right pane, the previous
conversation is replayed by the CLI, and they type the next instruction.

**UC-4 Something wants me.** Working in another app, the owner sees the hover's **waiting on you**
counter go from 0 to 1 and a row rise: `claude · demo-studio · session-console-monitor · needs
 you 2m`. They click it; Pigeon comes forward with that session selected and its embedded terminal
focused;
the permission prompt is on screen.

**UC-5 What did this project cost this week?** The owner opens Recent, expands `demo-studio`,
and selects the project. Its detail shows the seven-day qualifying sessions (claude 15 · codex 6 ·
opencode 2), counted 23 of 23, 48M cache read, 1.1M output, 2,140 API calls, 3,905 tool calls;
context per call 22k; rewrite ratio 0.09; batching 1.8.

**UC-6 Start work in a new project.** The owner clicks **New project**, chooses a folder anywhere in
the filesystem, then chooses **Open in Codex** from the project card. The Codex CLI starts in that
folder in the existing right pane; once it writes its first session record, the project appears in
Recent.

**UC-7 Codex hit its limit.** The Codex card shows no bars and the engine's own words: "Codex
reports a reached limit: workspace_member_credits_depleted", reading taken 15 minutes ago. Nothing
is drawn green.

**UC-8 An engine is not installed.** On the Windows box OpenCode is absent. The strip shows a quiet
`opencode: not installed` chip; the list contains only Claude and Codex rows; nothing errors.

**UC-9 Opened from Finder.** The owner launches from the Dock. Resume works because Pigeon
reconstructed the login-shell PATH; the terminal shows colours because pigeon set `TERM`.

**UC-10 Stop everywhere, resume later.** A Codex session is open in another terminal and appears
live in Pigeon. The owner selects it and clicks **Stop session**. Pigeon identifies the matching
Codex process, terminates it, removes it from Live, and leaves the rollout untouched. Later the owner
clicks **Resume** and the same session starts again in Pigeon's right pane.

**UC-11 Return to a Pigeon terminal.** The owner resumes a Claude session in Pigeon, selects a
different session, then returns. The same terminal process is still running; its previous display is
replayed from Pigeon's bounded scrollback and new output continues in place. Quitting Pigeon ends
that owned process.

## 7. Constraints and assumptions

- The Claude usage endpoint is undocumented and unsupported; its fields can change without notice
  (they already carry a dozen opaque buckets). Drift must surface as absence.
- Claude Code's per-process status file is observed behaviour of CLI 2.1.269; `claude agents
  --json` is the CLI's scripting surface for the same data. Status values seen: `busy`, `idle`;
  documented for scripting: `needs_input`. Strings in the binary suggest a wider set (`running`,
  `blocked`, `permission`, `waiting`, `exited`); pigeon maps only `busy`/`running` → running,
  `needs_input`/`blocked`/`permission`/`waiting` → needs you, `idle` → finished, and shows any other
  word verbatim rather than guessing.
- Codex approval prompts are not recorded in rollouts, so Codex "needs you" is read from the
  `PermissionRequest` hook (installed with consent, `docs/decisions/0002-codex-hooks.md`) and only
  while the rollout turn is open. Codex questions (`request_user_input`) still have no hook event
  and read **running**; stated on the badge's hover text.
- OpenCode's `permission` table held zero rows during a measured live `external_directory` approval
  (2026-09-18, an 8.6-minute wait): the table holds *saved rules*, and a pending ask lives only in
  the running process. Pigeon therefore offers to install a plugin bridge that records
  `permission.asked` / `permission.replied` with the session id
  (`docs/decisions/0004-opencode-permission-bridge.md`); without the install, a permission wait it
  cannot see reads **running** and the owner-wait policy returns a stated absence rather than a
  guess.
- Codex's `input_tokens` **includes** the cached portion; the studio subtracts it so the six
  counters carry Anthropic semantics (full prompt = input + cache read + cache write). Pigeon does
  the same, and fails loud if cached exceeds input.
- Subscription logins have no per-token price; pigeon shows none.
- Windows and Linux are compile targets only in the first pass.

## 8. Deferred and excluded

| Item | Why not now | Where it would go |
|---|---|---|
| Transcript history older than seven days | Recent deliberately omits it; Pigeon has no archive view | demo-studio |
| Multi-profile logins (`CLAUDE_CONFIG_DIR` / `CODEX_HOME` homes) | studio-scale machinery; owner uses the stock home on this Mac | studio |
| Codex approval detection via the app-server protocol | file evidence suffices for turn state; the protocol is a subprocess the studio needed a lane for | later, if the owner sees false "waiting" |
| OS notifications when a session turns **waiting on you** | the hover is the stated surface | Could, after the hover proves itself |
| Filesystem watching (FSEvents) | 5–10 s polling meets every acceptance above with no watcher thread | v2 if polling shows on the CPU |
| Transcript viewing, search, trends, history | the studio's job | studio |
| Signing and notarization on macOS | first pass runs from `tauri dev` and an unsigned `.app` | release work |
| Windows/Linux verification | Mac first by ruling | second pass |

## 9. Glossary

- **Engine** — a coding agent CLI pigeon reads and launches: Claude Code, Codex CLI, OpenCode.
- **Session** — one conversation the engine persisted: a Claude transcript, a Codex thread (one or
  more rollout files), an OpenCode session row.
- **Project** — the session's working directory, identified by its normalised absolute path,
  labeled by its leaf folder.
- **Console** — a PTY Pigeon owns, running one engine process in the right pane; it is not a visible tab.
- **Stop session** — a bounded, session-scoped process termination operation; it never deletes source
  records and refuses ambiguous matches.
- **Live status** — running · needs you · finished, or unknown when a live process's evidence is
  missing (UR-7). A session with no live process has no status. "Waiting on you" means needs-you or
  finished.
- **Capacity** — an engine's own statement of allowance used in its 5-hour and weekly windows.
- **The hover** — the always-on-top status window (UR-8).
- **Raw six** — input tokens, output tokens, cache read, cache write, API calls, tool calls.
- **The three KPIs** — context per call, rewrite ratio, batching ratio, as the studio defines them.

## 10. Traceability to the FRD

| UR | Functional requirements |
|---|---|
| UR-1 | FR-5, FR-6, FR-7, FR-10, FR-15, FR-24 |
| UR-2 | FR-11, FR-12, FR-13, FR-14, FR-15, FR-25, FR-26 |
| UR-3 | FR-1, FR-2, FR-3, FR-4, FR-5, FR-7, FR-25 |
| UR-4 | FR-8, FR-9, FR-10, FR-27, FR-28, FR-34 |
| UR-5 | FR-6, FR-16, FR-17, FR-18 |
| UR-6 | FR-7, FR-18, FR-34 |
| UR-7 | FR-19, FR-20, FR-21, FR-22 |
| UR-8 | FR-23 |
| UR-9 | FR-5, FR-7, FR-22, FR-24 |
| UR-10 | FR-25, FR-32, NFR-9 |
| UR-11 | FR-4, FR-30, NFR-2 |
| UR-12 | FR-11, FR-29, NFR-3 |
| UR-13 | FR-12, FR-13, FR-16, FR-26, NFR-4 |
| UR-14 | FR-27, FR-28, FR-32, NFR-6 |
| UR-15 | FR-1, FR-4, FR-21, NFR-7 |
| UR-16 | NFR-11 |
| UR-17 | FR-6, FR-15 (negative requirements) |
| UR-18 | FR-10, FR-35, NFR-13 |
| UR-19 | FR-32, FR-36, NFR-14 |

## Appendix A — Decision log, 2026-09-12

| # | Question put to the owner | Answer |
|---|---|---|
| 1 | What should account visibility show? | Login + capacity bars |
| 2 | Which engines? | Claude Code, Codex, **and OpenCode** now; xAI/Meta later |
| 3 | Keep Python as a thin sidecar? | **Drop Python** |
| 4 | Where does the terminal live? | In-pane, right side |
| 5 | Repo root? | `~/Documents/Projects/feather` |
| 6 | (owner addition) per-session numbers | six counters + three KPIs, "cost and usefulness" |
| 7 | (owner addition) project level | the same rolled up per project |
| 8 | (owner addition) OpenCode | first-class from the start |
| 9 | (owner addition) model name | not shown |
| 10 | Session scope | Live first; Recent means projects and sessions active within seven days |
| 11 | New sessions | Choose a folder, then start Claude, Codex, or OpenCode in the existing right pane |
| 12 | Process control | Pigeon-owned Stop session terminates every proven matching process, including external ones; ambiguous matches fail closed |
| 13 | Terminal continuity | Switching sessions detaches only the View; Pigeon retains bounded scrollback and closes all owned consoles on app exit |
| 14 | Refresh controls | Poll every 5 seconds by default; Recent covers 7 days by default; both are owner-configurable |
| 10 | (owner addition) live status | running / needs you / finished, in the hover and in the app; sessions with no process are not counted or badged |
