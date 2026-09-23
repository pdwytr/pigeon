# Implementation Plan: Unified Session Activity State

## Overview

Make session activity state coherent across the terminal, session rows, project cards, sidebar totals, detail pane, and hover. The host remains authoritative for engine state; React derives every visible projection from the latest status snapshot and the current session inventory. PTY lifecycle is kept as a separate concern and is never presented as proof that an agent is actively working.

## Current root causes

1. `TerminalPanel` renders `ConsoleSummary.state`, which describes the PTY (`running` means the child process exists), not the agent turn. An idle prompt therefore appears as “process running.”
2. Session rows rejoin the latest `StatusSnapshot`, but project cards and the sidebar use `ProjectSummary.statusCounts` from a separate request. Those answers can be from different observations and visibly disagree.
3. `console://data`, console input, and console lifecycle changes do not cause a status reconciliation. The terminal can change while the session/project state remains unchanged until an unrelated worker event or poll.
4. The product vocabulary is ambiguous: the backend maps Claude `idle` to `needs_you`, while the written contract says idle at the prompt is `finished` and `needs_you` is an actual blocking prompt/permission state.

## Architecture decisions

- Keep two explicit state domains:
  - `ConsolePhase`: attaching, live, exited, gone — PTY/terminal lifecycle.
  - `SessionStatus`: running, waiting/finished, needs-you, unknown, or absent — engine/session activity.
- Treat the latest accepted `StatusSnapshot` as the authority for live session status. Never infer `running` or `waiting` from absence/presence of terminal output.
- Add one pure frontend projection for status-reconciled rows and project status counts. All UI consumers use it; no component reads raw `ProjectSummary.statusCounts` for live activity.
- Preserve full `SessionKey` identity in all maps and joins.
- Make status and membership refreshes resilient to out-of-order responses. Existing reducer token/timestamp protections remain required.
- Resolve semantics before implementation: recommended mapping is `busy`/open turn → `running`, idle at an agent prompt → user-facing `waiting` (wire-compatible with current `finished` if renaming is deferred), and explicit permission/question block → `needs_you`. Unknown engine words remain `unknown` and retain the raw word/evidence.

## Task List

### Phase 1: Contract and regression coverage

#### Task 1: Write state invariants and failing regression tests

**Description:** Add focused tests that reproduce the current inconsistencies before changing implementation.

**Acceptance criteria:**

- [ ] A status transition from running to idle/waiting updates the session row, project count, sidebar count, and detail status consistently.
- [ ] A PTY remaining alive does not cause an idle session to render as actively running.
- [ ] A status-only update cannot leave project-card counts different from the session-row badges.
- [ ] A console activity/lifecycle event triggers a status reconciliation path without using output silence as a state signal.
- [ ] Tests cover both a keyed resumed console and an unkeyed newly started console.

**Verification:** Run the focused Vitest files and confirm the new regression tests fail against the current implementation.

**Dependencies:** None.

**Files likely touched:** `src/store/viewStore.test.ts`, `src/store/selectors.test.ts` (new if useful), `src/components/ProjectCard.test.tsx` (new if useful), `src/components/TerminalPanel.test.tsx`, `src/store/usePigeonApp.test.tsx`.

**Estimated scope:** Medium.

#### Task 2: Freeze the activity vocabulary and wire contract

**Description:** Decide and document the exact user-facing meaning of idle, completed-turn, explicit input-required, unknown, and exited states. Add serialization tests where the Rust/TypeScript contract changes.

**Acceptance criteria:**

- [ ] “PTY process alive” is not named or displayed as agent “running.”
- [ ] Idle-at-prompt and blocked-on-owner have distinct, documented meanings.
- [ ] Existing wire compatibility is preserved unless a deliberate enum migration is required.
- [ ] Recent/closed sessions never receive a live status from a stale snapshot.

**Verification:** Rust domain/status tests and TypeScript typecheck pass; the decision is recorded in the plan/contract docs.

**Dependencies:** Task 1.

**Files likely touched:** `docs/contracts/urds.md`, `docs/contracts/objects.md`, `docs/contracts/components.md`, `src/bindings.ts`, `src/components/StatusBadge.tsx`, `src-tauri/src/domain/status.rs`.

**Estimated scope:** Medium.

### Checkpoint: Contract baseline

- [ ] Regression tests demonstrate the current failures.
- [ ] Idle versus needs-you semantics are agreed and written down.
- [ ] No implementation changes have been mixed into the baseline tests.

### Phase 2: Single frontend projection

#### Task 3: Centralize status-reconciled row and project projections

**Description:** Extend the selector layer so the latest accepted live snapshot is joined by `SessionKey` once, then used to derive row status and per-project counts. Keep metrics/totals from host summaries; derive only activity fields in the frontend.

**Acceptance criteria:**

- [ ] `visibleSessions` and project status counts use the same status lookup.
- [ ] `WorkspaceSidebar` live total and `ProjectCard` live/closed labels cannot disagree with visible row badges.
- [ ] Missing live observations are handled according to the contract: absent status where the host cannot answer, and no fabricated live status in Recent.
- [ ] Status snapshots with duplicate or same-`sid` sessions from different providers remain isolated.

**Verification:** Selector/reducer tests pass, including out-of-order status snapshots and same-`sid` provider cases.

**Dependencies:** Tasks 1–2.

**Files likely touched:** `src/store/selectors.ts`, `src/components/WorkspaceSidebar.tsx`, `src/components/ProjectList.tsx`, `src/components/ProjectCard.tsx`, relevant tests.

**Estimated scope:** Medium.

#### Task 4: Make status updates atomic from the view’s perspective

**Description:** Update the reducer/wiring so a status event becomes the single state transition consumed by all selectors. Re-fetch session membership only when necessary; do not depend on a separately timed project-summary response to update activity counts.

**Acceptance criteria:**

- [ ] A `status://changed` event immediately updates all live activity consumers on the next render.
- [ ] A late session/project response cannot restore older activity counts.
- [ ] Membership changes still refresh Live rows and projects, including process start/exit.
- [ ] Loading/error state remains accurate and does not cause request loops.

**Verification:** Hook tests cover status-only events, membership events, overlapping refreshes, and failed requests. Run typecheck and Vitest.

**Dependencies:** Task 3.

**Files likely touched:** `src/store/viewStore.ts`, `src/store/usePigeonApp.ts`, `src/store/selectors.ts`, `src/store/usePigeonApp.test.tsx`, `src/store/viewStore.test.ts`.

**Estimated scope:** Medium.

### Checkpoint: Frontend state coherence

- [ ] One status transition produces identical state across row, card, sidebar, detail, and hover.
- [ ] No component independently computes live/working state from console presence.
- [ ] Focused tests and typecheck pass.

### Phase 3: Terminal and host reconciliation

#### Task 5: Correct terminal presentation and activity reconciliation

**Description:** Rename terminal header language to describe the PTY lifecycle (“terminal connected/ended” or equivalent), and connect meaningful terminal actions to a host status refresh. Do not classify state from quiet output, keystroke timing, or terminal phase.

**Acceptance criteria:**

- [ ] An idle but open terminal never displays “process running” as the agent state.
- [ ] Console open/resume, input, and exit cause a status reconciliation where a session key exists.
- [ ] New unkeyed consoles remain visibly attachable but are not assigned a fabricated session status before discovery.
- [ ] Closing a terminal does not silently mark the durable session finished; subsequent host status/membership data decides that.

**Verification:** Terminal and hook tests cover open, input, exit, detach/reattach, and console-list races. Manually verify a resumed session while it is working, idle, waiting for permission, and exited.

**Dependencies:** Task 4.

**Files likely touched:** `src/components/TerminalPanel.tsx`, `src/console/ConsoleView.tsx`, `src/store/usePigeonApp.ts`, terminal tests, API/event types if a callback is needed.

**Estimated scope:** Large; split further if the callback/event seam exceeds five files.

#### Task 6: Verify and correct backend state production

**Description:** Audit Claude, Codex, and OpenCode observations against the frozen vocabulary. Ensure a process being alive is only liveness evidence, while turn markers/status words decide activity. Add or update host events so status and session membership changes are emitted for terminal-start/stop transitions.

**Acceptance criteria:**

- [ ] Claude status words map to the documented states, with idle semantics matching Task 2.
- [ ] Codex open/closed turn markers distinguish active work from waiting.
- [ ] OpenCode attribution and permission evidence retain unknown/degraded behavior rather than guessing.
- [ ] Status and Live membership events are emitted when a terminal-created session becomes discoverable or exits.
- [ ] Existing stale-response and provider-isolation guarantees remain intact.

**Verification:** Run focused Rust status/service tests, then `cargo test`, `cargo clippy --all-targets -- -D warnings`, and the full frontend test suite.

**Dependencies:** Task 2 and Task 5.

**Files likely touched:** `src-tauri/src/services/status.rs`, provider adapters, `src-tauri/src/services/console.rs`, `src-tauri/src/lib.rs`, Rust tests/fixtures.

**Estimated scope:** Large; split by provider if implementation exceeds the task size.

### Phase 4: End-to-end review

#### Task 7: Runtime verification and cleanup

**Description:** Exercise the real Tauri app with multiple sessions/providers and verify state transitions at every surface. Remove temporary diagnostics and update the final contract notes.

**Acceptance criteria:**

- [ ] Working, idle/waiting, explicit needs-you, unknown, and exited states are all visually distinct and text-labeled.
- [ ] Left project/session state updates without manual tab switching or refresh.
- [ ] Project totals/counts remain stable while activity state changes.
- [ ] No duplicate requests, stale resurrection, or console reattachment regressions appear.

**Verification:** Full `npm run gate` plus browser/Tauri manual checks with console logs/network events captured for one transition per state.

**Dependencies:** Tasks 3–6.

**Files likely touched:** Tests, docs, and only the implementation files required by earlier tasks.

**Estimated scope:** Medium.

## Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Renaming `finished` to `waiting` breaks Rust/TS wire compatibility | High | Prefer a display-label change first; migrate the enum only with serialization tests and an explicit contract decision. |
| Inferring state from terminal output creates false waiting/running states | High | Keep host engine evidence authoritative; terminal events only trigger refreshes. |
| Status and session membership answers race | High | Preserve reducer tokens/timestamps and test response permutations. |
| Project summaries contain valid metrics/totals but stale statuses | Medium | Derive only status counts from reconciled rows; continue using host summaries for aggregate metrics. |
| A console starts before discovery supplies a session key | Medium | Keep it project-scoped/unkeyed until discovery; never invent identity from cwd alone. |
| Provider behavior differs or changes | Medium | Preserve raw engine words/evidence and map unknown words to unknown rather than guessing. |

## Open questions requiring confirmation before implementation

- Should the UI call the idle-at-prompt state “waiting” or retain the existing “finished” label? The recommended user-facing label is “waiting”; the safer compatibility path is to retain `finished` internally and change only display copy.
- For an explicit permission/question prompt, should the urgent label remain “needs you”? The recommended answer is yes, distinct from ordinary idle/waiting.

