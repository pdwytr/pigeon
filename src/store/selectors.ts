// Reads over `ViewState`. Pure, memo-free, and deliberately small.
//
// The rule every selector here is built to keep: **nothing is joined by `sid`.** Every lookup goes
// through `sessionKeyId`, because two engines minting the same uuid is normal, not exotic, and a
// join that collapsed them would show one session's metrics under another's name.
//
// The second rule: **a scope's data is only ever read from that scope's slice.** Switching from
// Recent to Live cannot invent a live status for a closed session, because `liveStateFor` consults
// the status snapshot only while the Live scope is showing, and the snapshot contains none but live
// sessions in the first place.

import type {
  ConsoleSummary,
  LiveSessionState,
  LiveState,
  MetricState,
  ProjectSummary,
  SessionKey,
  SessionRow,
  StatusCounts,
  StatusSnapshot,
} from "../bindings";
import { sessionKeyId } from "../bindings";
import type { ViewState } from "./viewStore";

/** The timestamp a row is ordered by: when it closed if it has, otherwise when it last moved. */
export function activityMs(row: SessionRow): number {
  return row.closedAtMs ?? row.lastActiveMs;
}

/**
 * Merge rows from every engine into one ordered list: most recent activity first, then provider,
 * then the complete sid. The tiebreakers matter — without them two rows written in the same
 * millisecond would swap places between renders and the list would flicker.
 */
export function sortSessions(rows: SessionRow[]): SessionRow[] {
  return [...rows].sort((a, b) => {
    const byTime = activityMs(b) - activityMs(a);
    if (byTime !== 0) return byTime;
    if (a.key.providerId !== b.key.providerId) return a.key.providerId < b.key.providerId ? -1 : 1;
    return a.key.sid < b.key.sid ? -1 : 1;
  });
}

/**
 * The rows for the current scope, with each status taken from the LIVE snapshot.
 *
 * A row arrives from `sessions_list` carrying the status that was true when that list was built.
 * `status://changed` then updates the snapshot several times a minute without rewriting the rows,
 * so a badge kept saying "running" after the engine had gone back to waiting — and the detail
 * pane, which reads the snapshot through `liveStateFor`, disagreed with the row beside it about
 * the same session.
 *
 * One source settles both. A session the snapshot does not mention is left with whatever the host
 * decided when it built the list: in Live that is a session the status pass could not speak for,
 * and in Recent the snapshot is not consulted at all, because a closed session has no live state
 * to find.
 */
export function visibleSessions(state: ViewState): SessionRow[] {
  const rows = state.sessions[state.scope];
  if (state.scope !== "live" || !state.status) return sortSessions(rows);
  const live = new Map(
    state.status.live.map((l) => [sessionKeyId(l.key), sessionStatusFromLiveState(l.state)]),
  );
  return sortSessions(rows.map((row) => reconcileLiveRow(row, live)));
}

function sessionStatusFromLiveState(state: LiveState): SessionRow["status"] {
  if (state === "waiting") return "finished";
  return state;
}

function reconcileLiveRow(row: SessionRow, live: Map<string, SessionRow["status"]>): SessionRow {
  const observed = live.get(sessionKeyId(row.key));
  return observed === undefined || observed === row.status ? row : { ...row, status: observed };
}

/** Activity counts for the rows currently visible to the owner.
 *
 * Project summaries also contain totals and metric rollups, but their status counts come from a
 * separate host read. Recomputing only this small projection from the same reconciled rows keeps
 * cards and row badges atomic without duplicating the host's metric aggregation work.
 */
export function statusCountsForRows(rows: SessionRow[]): StatusCounts {
  const counts: StatusCounts = { running: 0, needsYou: 0, finished: 0, unknown: 0 };
  for (const row of rows) {
    if (row.status === "running" || row.status === "delegating") counts.running += 1;
    else if (row.status === "needs_you") counts.needsYou += 1;
    else if (row.status === "finished") counts.finished += 1;
    else counts.unknown += 1;
  }
  return counts;
}

export function statusCountsForProject(state: ViewState, project: string): StatusCounts {
  return statusCountsForRows(visibleSessions(state).filter((row) => row.project === project));
}

export function visibleProjects(state: ViewState): ProjectSummary[] {
  return state.projects[state.scope];
}

export function sessionsForProject(rows: SessionRow[], project: string): SessionRow[] {
  return sortSessions(rows.filter((r) => r.project === project));
}

export function findSession(state: ViewState, key: SessionKey | null): SessionRow | null {
  if (!key) return null;
  const id = sessionKeyId(key);
  // The current scope first, then the other one: a session selected in Live and still selected
  // after a switch to Recent is findable, which is what lets the pane keep rendering it.
  const ordered = state.scope === "live" ? ["live", "recent"] : ["recent", "live"];
  for (const scope of ordered as ("live" | "recent")[]) {
    const rows = scope === state.scope ? visibleSessions(state) : state.sessions[scope];
    const found = rows.find((r) => sessionKeyId(r.key) === id);
    if (found) return found;
  }
  return null;
}

/** Is the selected session present in the scope currently on screen? */
export function inCurrentScope(state: ViewState, key: SessionKey): boolean {
  const id = sessionKeyId(key);
  return state.sessions[state.scope].some((r) => sessionKeyId(r.key) === id);
}

export function findProject(state: ViewState, project: string | null): ProjectSummary | null {
  if (!project) return null;
  return visibleProjects(state).find((p) => p.project === project) ?? null;
}

export function metricsFor(state: ViewState, key: SessionKey | null): MetricState | null {
  if (!key) return null;
  return state.metrics[sessionKeyId(key)] ?? null;
}

/**
 * The host's live observation of a session, or `null`.
 *
 * Gated on the Live scope on purpose. A closed session is never in a `StatusSnapshot`, but a stale
 * snapshot arriving while Recent is on screen must not be allowed to paint a "running" badge onto a
 * row the user is looking at precisely because it is finished.
 */
export function liveStateFor(state: ViewState, key: SessionKey | null): LiveSessionState | null {
  if (!key || state.scope !== "live" || !state.status) return null;
  const id = sessionKeyId(key);
  return state.status.live.find((l) => sessionKeyId(l.key) === id) ?? null;
}

/** The console the host holds for this session, attached or detached. Matched by complete key. */
export function consoleForSession(state: ViewState, key: SessionKey | null): ConsoleSummary | null {
  if (!key) return null;
  const id = sessionKeyId(key);
  for (const summary of Object.values(state.consoles)) {
    if (!summary.sessionKey) continue;
    if (sessionKeyId(summary.sessionKey) !== id) continue;
    if (summary.state === "closed") continue;
    return summary;
  }
  return null;
}

/**
 * The console the host holds for a PROJECT: one with no session of its own, started in that
 * folder by "Add session". The engine has not written a discoverable record yet, so there is no
 * `SessionKey` to match on and the working directory is the only identity it has.
 */
export function consoleForProject(state: ViewState, cwd: string | null): ConsoleSummary | null {
  if (!cwd) return null;
  for (const summary of Object.values(state.consoles)) {
    if (summary.sessionKey) continue;
    if (summary.state === "closed") continue;
    if (summary.cwd !== cwd) continue;
    return summary;
  }
  return null;
}

export function selectedKey(state: ViewState): SessionKey | null {
  return state.selection.kind === "session" ? state.selection.key : null;
}

/** The project a selection implies: the project itself, or the selected session's project. */
export function selectedProjectKey(state: ViewState): string | null {
  if (state.selection.kind === "project") return state.selection.project;
  if (state.selection.kind === "session") {
    return findSession(state, state.selection.key)?.project ?? null;
  }
  return null;
}

/**
 * The rows the live hover shows, given the host's two answers.
 *
 * **Filtered to the status snapshot, not merely to the Live scope.** The hover's summary counts come
 * from `StatusSnapshot.counts`, and the contract requires that those counts equal the rows beneath
 * them (§12). A live session the status service has not observed yet is in the Live LIST but not in
 * the snapshot, so showing it would put four rows under a headline that adds up to three — and the
 * owner would be left working out which row the counts disagree about. With no snapshot at all the
 * hover has no rows to show; the surface says whether that is because it is still loading, because
 * the read failed, or because nothing is running.
 *
 * It takes the two answers rather than a `ViewState` because the hover is its own window with its
 * own two requests and no store — and a selector nothing calls is a rule nothing enforces.
 */
export function hoverSessions(status: StatusSnapshot | null, rows: SessionRow[]): SessionRow[] {
  if (!status) return [];
  const live = new Map(
    status.live.map((observation) => [
      sessionKeyId(observation.key),
      sessionStatusFromLiveState(observation.state),
    ]),
  );
  return sortSessions(
    rows.filter((row) => live.has(sessionKeyId(row.key))).map((row) => reconcileLiveRow(row, live)),
  );
}
