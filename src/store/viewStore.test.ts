import { describe, expect, it } from "vitest";
import { summarize } from "../api/fake";
import type {
  ProjectSummaryResult,
  SessionListResult,
  SessionRow,
  StatusSnapshot,
} from "../bindings";
import { makeRow, T0 } from "../test/rows";
import {
  hoverSessions,
  liveStateFor,
  statusCountsForProject,
  statusCountsForRows,
  visibleSessions,
} from "./selectors";
import { initialState, reducer, type ViewState } from "./viewStore";

function sessionsResult(over: Partial<SessionListResult> = {}): SessionListResult {
  return {
    scope: "live",
    sinceMs: null,
    rows: [],
    problems: [],
    generatedAtMs: T0,
    ...over,
  };
}

function projectsResult(over: Partial<ProjectSummaryResult> = {}): ProjectSummaryResult {
  return { scope: "live", sinceMs: null, projects: [], generatedAtMs: T0, ...over };
}

function apply(state: ViewState, ...actions: Parameters<typeof reducer>[1][]): ViewState {
  return actions.reduce(reducer, state);
}

describe("a late answer for a scope that has since been asked again", () => {
  it("is ignored, and does not replace the newer data", () => {
    const stale = makeRow({ sid: "stale", title: "From the first request" });
    const fresh = makeRow({ sid: "fresh", title: "From the second request" });

    // Two requests go out; the SECOND answers first, and the first answer arrives afterwards.
    const state = apply(
      initialState("live"),
      { type: "sessions/request", scope: "live", token: 1 },
      { type: "sessions/request", scope: "live", token: 2 },
      { type: "sessions/loaded", token: 2, result: sessionsResult({ rows: [fresh] }) },
      { type: "sessions/loaded", token: 1, result: sessionsResult({ rows: [stale] }) },
    );

    expect(state.sessions.live.map((r) => r.title)).toEqual(["From the second request"]);
  });

  it("ignores a late project summary for the same reason", () => {
    const state = apply(
      initialState("live"),
      { type: "projects/request", scope: "live", token: 1 },
      { type: "projects/request", scope: "live", token: 2 },
      { type: "projects/loaded", token: 2, result: projectsResult({ generatedAtMs: 2 }) },
      { type: "projects/loaded", token: 1, result: projectsResult({ generatedAtMs: 1 }) },
    );

    expect(state.projects.generatedAtMs.live).toBe(2);
  });

  it("keeps each scope's tokens separate, so a Recent answer cannot cancel a Live one", () => {
    const live = makeRow({ sid: "live-row", title: "Live" });
    const state = apply(
      initialState("live"),
      { type: "sessions/request", scope: "live", token: 1 },
      { type: "sessions/request", scope: "recent", token: 2 },
      {
        type: "sessions/loaded",
        token: 1,
        result: sessionsResult({ scope: "live", rows: [live] }),
      },
    );

    expect(state.sessions.live).toHaveLength(1);
  });
});

describe("switching from Recent to Live", () => {
  const closed = makeRow({
    sid: "closed-one",
    title: "Console polish",
    status: "finished",
    lastActiveMs: T0 - 10_800_000,
    closedAtMs: T0 - 10_800_000,
  });
  const running = makeRow({
    provider: "codex",
    sid: "live-one",
    title: "Fix tests",
    status: "running",
  });

  function loaded(): ViewState {
    return apply(
      initialState("live"),
      { type: "sessions/request", scope: "recent", token: 1 },
      {
        type: "sessions/loaded",
        token: 1,
        result: sessionsResult({ scope: "recent", rows: [closed] }),
      },
      { type: "sessions/request", scope: "live", token: 2 },
      {
        type: "sessions/loaded",
        token: 2,
        result: sessionsResult({ scope: "live", rows: [running] }),
      },
    );
  }

  it("never shows a closed session among the live rows", () => {
    const state = loaded();
    expect(visibleSessions(state).map((r) => r.title)).toEqual(["Fix tests"]);

    const recent = reducer(state, { type: "scope/set", scope: "recent" });
    expect(visibleSessions(recent).map((r) => r.title)).toEqual(["Console polish"]);

    const back = reducer(recent, { type: "scope/set", scope: "live" });
    expect(visibleSessions(back).map((r) => r.title)).toEqual(["Fix tests"]);
  });

  it("never invents a live observation for a closed session, even from a stale snapshot", () => {
    // A snapshot that still names the closed session — which the host would never send, and which
    // the View must not honour anyway while the Recent scope is on screen.
    const snapshot: StatusSnapshot = {
      generatedAtMs: T0,
      counts: { running: 1, needsYou: 0, unknown: 0 },
      live: [
        {
          key: closed.key,
          process: "present",
          state: "running",
          projectLeaf: closed.projectLeaf,
          title: closed.title,
          name: closed.name,
          sinceMs: T0,
          rawWord: "working",
          evidence: [],
          pid: 1,
          consoleId: null,
          activeSubagents: 0,
          observedAtMs: T0,
        },
      ],
    };

    const recent = apply(
      loaded(),
      { type: "status/set", snapshot },
      { type: "scope/set", scope: "recent" },
    );
    expect(liveStateFor(recent, closed.key)).toBeNull();
  });
});

describe("the console registry", () => {
  it("drops a console the host no longer lists, and stops showing it", () => {
    // A closed console is REMOVED from the host's registry rather than marked closed, so
    // disappearing from `console_list` is the signal that it ended.
    const summary = {
      id: "c1",
      sessionKey: null,
      provider: "codex" as const,
      cwd: "/tmp",
      mode: "new" as const,
      state: "running" as const,
      cols: 80,
      rows: 24,
      scrollbackBytes: 0,
      exitCode: null,
      startedAtMs: T0,
    };

    const withConsole = apply(
      initialState("live"),
      { type: "consoles/set", token: 1, consoles: [summary] },
      { type: "console/visible", id: "c1" },
    );
    expect(withConsole.visibleConsoleId).toBe("c1");

    const gone = reducer(withConsole, { type: "consoles/set", token: 2, consoles: [] });
    expect(gone.consoles).toEqual({});
    expect(gone.visibleConsoleId).toBeNull();
  });
});

describe("metrics", () => {
  it("merges by the complete session key, so two engines sharing a sid keep separate numbers", () => {
    const sid = "0199c4a1-2b3d-7e4f-8a9b-0c1d2e3f4a5b";
    const state = reducer(initialState("live"), {
      type: "metrics/merge",
      rows: [
        { key: { providerId: "claude-code", sid }, metrics: { state: "pending" } },
        {
          key: { providerId: "codex", sid },
          metrics: {
            state: "unavailable",
            error: { provider: "codex", kind: "busy", detail: { type: "none" }, message: "busy" },
          },
        },
      ],
    });

    expect(state.metrics[`claude-code:${sid}`].state).toBe("pending");
    expect(state.metrics[`codex:${sid}`].state).toBe("unavailable");
  });
});

describe("the live hover's population", () => {
  it("shows exactly the sessions the status snapshot counted, so rows and counts agree", () => {
    const observed = makeRow({ sid: "seen", title: "Observed", status: "running" });
    const unobserved = makeRow({ provider: "codex", sid: "unseen", title: "Not yet observed" });
    const status: StatusSnapshot = {
      generatedAtMs: T0,
      counts: { running: 1, needsYou: 0, unknown: 0 },
      live: [
        {
          key: observed.key,
          process: "present",
          state: "running",
          projectLeaf: observed.projectLeaf,
          title: observed.title,
          name: observed.name,
          sinceMs: T0,
          rawWord: "working",
          evidence: [],
          pid: 1,
          consoleId: null,
          activeSubagents: 0,
          observedAtMs: T0,
        },
      ],
    };

    const rows = hoverSessions(status, [observed, unobserved]);
    expect(rows.map((r) => r.title)).toEqual(["Observed"]);
    expect(rows).toHaveLength(status.counts.running);
  });

  it("is empty before the first snapshot rather than guessing from the session list", () => {
    expect(hoverSessions(null, [makeRow()])).toEqual([]);
  });

  it("reconciles each row's status from the latest live observation", () => {
    const row = makeRow({ sid: "state-change", status: "finished" });
    const status: StatusSnapshot = {
      generatedAtMs: T0,
      counts: { running: 1, needsYou: 0, unknown: 0 },
      live: [
        {
          key: row.key,
          process: "present",
          state: "running",
          projectLeaf: row.projectLeaf,
          title: row.title,
          name: row.name,
          sinceMs: T0,
          rawWord: "busy",
          evidence: [],
          pid: 1,
          consoleId: null,
          activeSubagents: 0,
          observedAtMs: T0,
        },
      ],
    };

    expect(hoverSessions(status, [row])[0].status).toBe("running");
  });
});

describe("a status snapshot that arrives out of order", () => {
  it("is dropped rather than allowed to resurrect a session the owner has stopped", () => {
    // `status_snapshot` carries no request token, so the only thing separating a fresh answer
    // from a slow one is the timestamp the host stamped on it.
    const older: StatusSnapshot = {
      generatedAtMs: T0 - 1_000,
      counts: { running: 1, needsYou: 0, unknown: 0 },
      live: [
        {
          key: { providerId: "claude-code", sid: "stopped-one" },
          process: "present",
          state: "running",
          projectLeaf: "pigeon",
          title: "Stopped a moment ago",
          name: "Stopped a moment ago",
          sinceMs: T0,
          rawWord: "working",
          evidence: [],
          pid: 1,
          consoleId: null,
          activeSubagents: 0,
          observedAtMs: T0,
        },
      ],
    };
    const newer: StatusSnapshot = {
      generatedAtMs: T0,
      counts: { running: 0, needsYou: 0, unknown: 0 },
      live: [],
    };

    const state = apply(
      initialState("live"),
      { type: "status/set", snapshot: newer },
      { type: "status/set", snapshot: older },
    );

    expect(state.status?.generatedAtMs).toBe(T0);
    expect(state.status?.live).toHaveLength(0);
  });
});

describe("a console list that arrives out of order", () => {
  it("is dropped, so a console the owner closed is not restored by a slow answer", () => {
    const summary = {
      id: "c1",
      sessionKey: null,
      provider: "codex" as const,
      cwd: "/tmp",
      mode: "new" as const,
      state: "running" as const,
      cols: 80,
      rows: 24,
      scrollbackBytes: 0,
      exitCode: null,
      startedAtMs: T0,
    };

    // Two `console_list` requests in flight; the SECOND answers first and says the console is
    // gone, then the first arrives still holding it.
    const state = apply(
      initialState("live"),
      { type: "consoles/set", token: 2, consoles: [] },
      { type: "consoles/set", token: 1, consoles: [summary] },
    );

    expect(state.consoles).toEqual({});
  });
});

describe("a live project card the owner collapsed", () => {
  it("stays collapsed across the next refresh", () => {
    // Live cards DEFAULT expanded (§3.2). Re-asserting that union on every `projects/loaded`
    // made the default a law: collapsing a card popped it open again five seconds later.
    const row = makeRow({ sid: "one", title: "A session" });
    const result = projectsResult({ projects: summarize([row]) });
    const project = result.projects[0].project;

    const first = apply(
      initialState("live"),
      { type: "projects/request", scope: "live", token: 1 },
      { type: "projects/loaded", token: 1, result },
    );
    expect(first.expandedProjects.has(project)).toBe(true);

    const collapsed = reducer(first, { type: "project/toggle", project });
    expect(collapsed.expandedProjects.has(project)).toBe(false);

    const refreshed = apply(
      collapsed,
      { type: "projects/request", scope: "live", token: 2 },
      { type: "projects/loaded", token: 2, result },
    );
    expect(refreshed.expandedProjects.has(project)).toBe(false);
  });

  it("still expands a project the owner has never seen before", () => {
    const first = makeRow({ sid: "one", project: "/a", projectName: "a" });
    const second = makeRow({ sid: "two", project: "/b", projectName: "b" });

    const state = apply(
      initialState("live"),
      { type: "projects/request", scope: "live", token: 1 },
      {
        type: "projects/loaded",
        token: 1,
        result: projectsResult({ projects: summarize([first]) }),
      },
      { type: "project/toggle", project: "/a" },
      { type: "projects/request", scope: "live", token: 2 },
      {
        type: "projects/loaded",
        token: 2,
        result: projectsResult({ projects: summarize([first, second]) }),
      },
    );

    expect(state.expandedProjects.has("/a")).toBe(false);
    expect(state.expandedProjects.has("/b")).toBe(true);
  });
});

describe("a row's status", () => {
  const liveRow = makeRow({ sid: "watched", title: "A session being watched" });

  /** A Live row that arrived from `sessions_list` saying "running". */
  function stateWith(rowStatus: SessionRow["status"], snapshot: StatusSnapshot | null): ViewState {
    const base = initialState("live");
    const row: SessionRow = { ...liveRow, status: rowStatus };
    return {
      ...base,
      scope: "live",
      sessions: { ...base.sessions, live: [row] },
      status: snapshot,
    };
  }

  function snapshotOf(state: "running" | "needs_you"): StatusSnapshot {
    return {
      generatedAtMs: 2_000,
      counts: {
        running: state === "running" ? 1 : 0,
        needsYou: state === "needs_you" ? 1 : 0,
        unknown: 0,
      },
      live: [
        {
          key: liveRow.key,
          process: "present",
          state,
          projectLeaf: null,
          title: null,
          name: null,
          sinceMs: null,
          rawWord: state === "running" ? "busy" : "idle",
          evidence: [],
          pid: 4123,
          consoleId: null,
          activeSubagents: 0,
          observedAtMs: 2_000,
        },
      ],
    };
  }

  it("follows the live snapshot rather than the list it arrived in", () => {
    // `status://changed` fires several times a minute without rewriting the rows, so a badge went
    // on saying "running" after the engine had gone back to waiting — and the detail pane, which
    // reads the snapshot, disagreed with the row beside it about one session.
    const rows = visibleSessions(stateWith("running", snapshotOf("needs_you")));

    expect(rows[0].status).toBe("needs_you");
  });

  it("leaves a row the snapshot does not mention exactly as the host decided it", () => {
    // In Live that is a session the status pass could not speak for; inventing a state for it is
    // the confident-wrong-answer this product exists not to give.
    const empty: StatusSnapshot = {
      generatedAtMs: 2_000,
      counts: { running: 0, needsYou: 0, unknown: 0 },
      live: [],
    };
    const rows = visibleSessions(stateWith(null, empty));

    expect(rows[0].status).toBeNull();
  });

  it("never consults the snapshot in Recent, where a closed session has no live state", () => {
    const base = initialState("recent");
    const recent: ViewState = {
      ...base,
      scope: "recent",
      sessions: { ...base.sessions, recent: [{ ...liveRow, status: "finished" }] },
      status: snapshotOf("running"),
    };

    expect(visibleSessions(recent)[0].status).toBe("finished");
  });

  it("derives project and sidebar counts from the reconciled row status", () => {
    const first = makeRow({ sid: "first", project: "/project", status: "running" });
    const second = makeRow({ sid: "second", project: "/project", status: "needs_you" });
    const state: ViewState = {
      ...initialState("live"),
      sessions: {
        ...initialState("live").sessions,
        live: [first, second],
      },
      status: null,
    };

    const rows = visibleSessions(state);
    expect(statusCountsForRows(rows)).toMatchObject({ running: 1, needsYou: 1 });
    expect(statusCountsForProject(state, "/project")).toMatchObject({ running: 1, needsYou: 1 });
  });
});
