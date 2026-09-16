// The wiring: api in, `ViewState` out, plus the actions components dispatch.
//
// Everything asynchronous lives here and nowhere else — startup, refresh, event subscriptions, the
// lazy metric fetch, and the console reconciliation. Components receive plain data and plain
// callbacks, which is what lets most of them be tested by rendering them with a literal.
//
// The two rules worth reading the code for:
//
//   * **Every scoped fetch is issued with a token** (`tokenRef`), and the reducer drops any answer
//     whose token is no longer the newest for that scope. Tabbing Live → Recent → Live twice in a
//     second is an ordinary thing to do, and without this the slowest of those three answers wins.
//
//   * **Selecting a session never closes a console.** Selection only changes which console is
//     VISIBLE; the host keeps the PTY, and coming back re-adopts it from `console_list` and replays
//     its scrollback. `console_close` happens when the owner asks, and at no other time.

import { useCallback, useEffect, useMemo, useReducer, useRef } from "react";
import type { FeatherApi } from "../api/types";
import type {
  EngineError,
  ProjectSummary,
  ProviderId,
  Scope,
  SessionKey,
  SessionRow,
} from "../bindings";
import { sameSession, sessionKeyId } from "../bindings";
import {
  consoleForProject,
  consoleForSession,
  findProject,
  inCurrentScope,
  selectedKey,
} from "./selectors";
import { type Action, initialState, reducer, type ViewState } from "./viewStore";

/** The geometry a console is LAUNCHED with. The attached terminal measures itself and sends a real
 *  `console_resize` within milliseconds, so this only has to be sane, not correct. */
const LAUNCH_COLS = 80;
const LAUNCH_ROWS = 24;

function asEngineError(err: unknown, provider: ProviderId | null = null): EngineError {
  return {
    provider,
    kind: "transport",
    detail: { type: "none" },
    // The host builds real messages from typed details; this is the last-resort wrapper for a
    // rejection that never reached the host at all.
    message: err instanceof Error ? err.message : "The host did not answer.",
  };
}

export interface FeatherActions {
  setScope(scope: Scope): void;
  selectProject(project: string): void;
  selectSession(key: SessionKey): void;
  clearSelection(): void;
  toggleProject(project: string): void;
  refresh(): void;
  openFolder(cwd: string): void;
  addSession(project: ProjectSummary, provider: ProviderId): void;
  resume(session: SessionRow): void;
  stop(session: SessionRow): void;
  closeConsole(id: string): void;
  refreshMetrics(key: SessionKey): void;
  refreshStatus(): void;
  toggleHover(): void;
  setHoverCompact(compact: boolean): void;
  pickProject(): Promise<string | null>;
  openProject(cwd: string): void;
  dismissNotice(id: string): void;
}

export interface FeatherApp {
  state: ViewState;
  actions: FeatherActions;
  /** Which visible console should replay its scrollback: true when it was adopted from
   *  `console_list`, false when this pane opened it a moment ago and there is nothing to replay. */
  replayVisible: boolean;
}

export function useFeatherApp(api: FeatherApi, pollMs = 0): FeatherApp {
  const [state, dispatch] = useReducer(reducer, undefined, () => initialState("live"));
  const tokenRef = useRef({ sessions: 0, projects: 0, consoles: 0 });
  /** Which scopes have ever been ASKED for, as opposed to which have answered. A ref and not
   *  state, because the follow-up effect below closes over the render that preceded its own
   *  dispatch and would otherwise ask a second time for everything. */
  const askedRef = useRef({ sessions: new Set<Scope>(), projects: new Set<Scope>() });
  /** Consoles this session opened itself and has not yet attached. They have no history worth
   *  replaying — for their FIRST attach, and only that one. */
  const freshConsoles = useRef(new Set<string>());
  /** The replay decision, frozen for as long as one console is the visible one. `ConsoleView`
   *  re-runs its whole attach when `replay` changes, so this must not flip under a mounted
   *  panel — it is recomputed when the visible console changes, and never in between. */
  const replayRef = useRef<{ id: string | null; replay: boolean }>({ id: null, replay: false });
  /** The selection the console reconciler last acted on, so it does not re-adopt on every render. */
  const reconciledFor = useRef<string | null>(null);
  const stateRef = useRef(state);
  stateRef.current = state;

  const loadSessions = useCallback(
    async (scope: Scope, force: boolean) => {
      tokenRef.current.sessions += 1;
      askedRef.current.sessions.add(scope);
      const token = tokenRef.current.sessions;
      dispatch({ type: "sessions/request", scope, token });
      try {
        const result = await api.sessionsList({ scope, force });
        dispatch({ type: "sessions/loaded", token, result });
      } catch (err) {
        dispatch({ type: "sessions/failed", scope, token, error: asEngineError(err) });
      }
    },
    [api],
  );

  const loadProjects = useCallback(
    async (scope: Scope) => {
      tokenRef.current.projects += 1;
      askedRef.current.projects.add(scope);
      const token = tokenRef.current.projects;
      dispatch({ type: "projects/request", scope, token });
      try {
        const result = await api.projectsSummary({ scope });
        dispatch({ type: "projects/loaded", token, result });
      } catch (err) {
        dispatch({ type: "projects/failed", scope, token, error: asEngineError(err) });
      }
    },
    [api],
  );

  const loadStatus = useCallback(async () => {
    try {
      dispatch({ type: "status/set", snapshot: await api.statusSnapshot() });
    } catch {
      // A status snapshot that cannot be read leaves the previous one in place. Replacing it with
      // an empty one would say "nothing is running", which is a claim, not an absence.
    }
  }, [api]);

  const loadConsoles = useCallback(async () => {
    // Tokened like the scoped loaders. `console_list` has no token of its own on the wire, and two
    // are in flight whenever a close races a refresh; the older answer still holds the console
    // that was just closed, and adopting it would attach the pane to a dead id.
    tokenRef.current.consoles += 1;
    const token = tokenRef.current.consoles;
    try {
      dispatch({ type: "consoles/set", token, consoles: (await api.consoleList()).consoles });
    } catch {
      // Same posture: an unanswered `console_list` is not evidence that no console exists.
    }
  }, [api]);

  // ---------------------------------------------------------------------------------- startup

  useEffect(() => {
    let cancelled = false;
    // Contract §4.1: these go out in parallel and the shell renders immediately behind them.
    void (async () => {
      const [host, settings] = await Promise.allSettled([api.hostInfo(), api.settingsGet()]);
      if (cancelled) return;
      if (host.status === "fulfilled") dispatch({ type: "host/info", os: host.value.os });
      if (settings.status === "fulfilled")
        dispatch({ type: "settings/set", settings: settings.value });
    })();
    void loadSessions("live", false);
    void loadProjects("live");
    void loadStatus();
    void loadConsoles();
    // Account/capacity is explicitly NOT a startup dependency (components.md §4.1) — it is fetched
    // after the dashboard has something to show, and its absence changes nothing on screen.
    void (async () => {
      try {
        const result = await api.accountStatus();
        if (!cancelled) dispatch({ type: "accounts/set", accounts: result.accounts });
      } catch {
        /* the capacity section simply does not render */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [api, loadSessions, loadProjects, loadStatus, loadConsoles]);

  // ----------------------------------------------------------------------------- subscriptions

  useEffect(() => {
    const stops = [
      api.onSessionsChanged((e) => {
        void loadSessions(e.scope, false);
        void loadProjects(e.scope);
      }),
      api.onSessionsMetrics((e) => dispatch({ type: "metrics/merge", rows: e.rows })),
      api.onStatusChanged((snapshot) => dispatch({ type: "status/set", snapshot })),
      api.onConsoleExit((e) => {
        dispatch({ type: "console/exited", id: e.id, exitCode: e.exitCode });
        void loadStatus();
      }),
      // The hover's click lands here: focus the row the owner pointed at, whichever scope it is in.
      api.onSelectSession((key) => dispatch({ type: "select/session", key })),
    ];
    return () => {
      for (const stop of stops) stop();
    };
  }, [api, loadSessions, loadProjects, loadStatus]);

  // --------------------------------------------------------------------------- scope follow-up

  // A scope the store has NEVER asked about gets one request. Both halves of that sentence are
  // load-bearing and both were wrong here:
  //
  //   * "never asked" is a fact about requests, so it is read from a ref. Read from `state`, the
  //     effect saw the render that preceded its own `sessions/request` dispatch and asked for
  //     everything a second time — four times under StrictMode.
  //   * a REJECTION is an answer. `sessions/failed` clears `loading` and leaves `generatedAtMs`
  //     null, so a condition written as "no data and not loading" is satisfied by the very state a
  //     failure produces: fail, refetch, fail, forever, with a token bump per cycle. A scope that
  //     has been answered badly is retried by Refresh, by a scope change, or by an event — never
  //     by this effect.
  useEffect(() => {
    if (!askedRef.current.sessions.has(state.scope)) void loadSessions(state.scope, false);
    if (!askedRef.current.projects.has(state.scope)) void loadProjects(state.scope);
  }, [state.scope, loadSessions, loadProjects]);

  // ------------------------------------------------------------------------ lazy session metrics

  const selected = selectedKey(state);
  const selectedId = selected ? sessionKeyId(selected) : null;
  useEffect(() => {
    if (!selected || !selectedId) return;
    const known = stateRef.current.metrics[selectedId];
    if (known && known.state !== "pending") return;
    let cancelled = false;
    void (async () => {
      try {
        const metrics = await api.sessionMetrics({ key: selected });
        if (!cancelled) dispatch({ type: "metrics/merge", rows: [{ key: selected, metrics }] });
      } catch {
        /* the pane keeps whatever it had; a failed fetch is not an unavailable metric */
      }
    })();
    return () => {
      cancelled = true;
    };
    // `selectedId` is the identity; `selected` is a fresh object on every render and would loop.
  }, [api, selectedId, selected]);

  // --------------------------------------------------------------- console adoption on selection

  const selectedProject = state.selection.kind === "project" ? state.selection.project : null;
  useEffect(() => {
    // What this selection's console is, if the host holds one. A session's is matched by its
    // complete key; a PROJECT's is a console with no session at all — "Add session" starts an
    // engine before any discoverable record exists, and the folder is the only identity it has
    // until one does. Without this second case that console became unreachable the moment the
    // owner looked at anything else: still running, with no panel and no Close button.
    const mark = selectedId ?? (selectedProject ? `project:${selectedProject}` : null);
    if (reconciledFor.current === mark) return;
    reconciledFor.current = mark;

    const existing = selectedId
      ? consoleForSession(stateRef.current, selected)
      : consoleForProject(
          stateRef.current,
          findProject(stateRef.current, selectedProject)?.cwd ?? selectedProject,
        );
    // Note what does NOT happen to the console we are leaving: nothing. It keeps running.
    dispatch({ type: "console/visible", id: existing?.id ?? null });
  }, [selectedId, selected, selectedProject]);

  // ------------------------------------------------------------------ the hover's real visibility

  // The hover is a separate window and can be closed from its own ×, which tells this window
  // nothing: the wire contract has no visibility event, so "Hide hover" stayed on the button and
  // clicking it made the hover APPEAR. Settings are where the host records the truth, so they are
  // re-read whenever this window comes back to the front — the moment the owner is about to look
  // at the button. A host-emitted `hover://visibility` event would be the proper fix and belongs
  // in the contract, not in a workaround here.
  useEffect(() => {
    const onFocus = () => {
      void (async () => {
        try {
          const settings = await api.settingsGet();
          dispatch({ type: "settings/set", settings });
        } catch {
          /* the button keeps what it had; a failed read is not a visibility change */
        }
      })();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [api]);

  // ---------------------------------------------------------------------------------- polling

  useEffect(() => {
    if (!pollMs) return;
    const timer = setInterval(() => void loadStatus(), pollMs);
    return () => clearInterval(timer);
  }, [pollMs, loadStatus]);

  // ---------------------------------------------------------------------------------- actions

  const actions = useMemo<FeatherActions>(() => {
    const notice = (id: string, text: string, tone: "info" | "error" = "error") =>
      dispatch({ type: "notice/push", notice: { id, tone, text } });

    return {
      setScope(scope) {
        dispatch({ type: "scope/set", scope });
        void loadSessions(scope, false);
        void loadProjects(scope);
      },
      selectProject(project) {
        dispatch({ type: "select/project", project });
      },
      selectSession(key) {
        dispatch({ type: "select/session", key });
      },
      clearSelection() {
        dispatch({ type: "select/clear" });
      },
      toggleProject(project) {
        dispatch({ type: "project/toggle", project });
      },
      refresh() {
        dispatch({ type: "refresh/start" });
        void (async () => {
          const scope = stateRef.current.scope;
          await Promise.allSettled([
            loadSessions(scope, true),
            loadProjects(scope),
            loadStatus(),
            loadConsoles(),
          ]);
          dispatch({ type: "refresh/end", atMs: Date.now() });
        })();
      },
      openFolder(cwd) {
        void api.folderOpen({ cwd }).catch(() => notice("folder", `Pigeon could not open ${cwd}.`));
      },
      addSession(project, provider) {
        // Look at the project the engine is starting in. The console it creates has no session of
        // its own — the engine has not written a discoverable record yet — so the PROJECT pane is
        // the only place that can render it; asked for from a sidebar card while a session was
        // selected, it would otherwise start a PTY with nowhere to appear.
        dispatch({ type: "select/project", project: project.project });
        void (async () => {
          try {
            const { id } = await api.sessionStart({
              provider,
              cwd: project.cwd,
              cols: LAUNCH_COLS,
              rows: LAUNCH_ROWS,
            });
            freshConsoles.current.add(id);
            await loadConsoles();
            void loadStatus();
            dispatch({ type: "console/visible", id });
          } catch (err) {
            notice("session-start", asEngineError(err, provider).message);
          }
        })();
      },
      resume(session) {
        void (async () => {
          try {
            const existing = consoleForSession(stateRef.current, session.key);
            if (existing) {
              dispatch({ type: "console/visible", id: existing.id });
              return;
            }
            const { id } = await api.consoleOpen({
              sessionKey: session.key,
              cwd: session.cwd ?? session.project,
              cols: LAUNCH_COLS,
              rows: LAUNCH_ROWS,
            });
            freshConsoles.current.add(id);
            await loadConsoles();
            void loadStatus();
            dispatch({ type: "console/visible", id });
          } catch (err) {
            notice("resume", asEngineError(err, session.key.providerId).message);
          }
        })();
      },
      stop(session) {
        void (async () => {
          try {
            const result = await api.sessionStop({ key: session.key });
            if (result.ambiguous.length) {
              notice(
                "stop",
                `Pigeon refused to stop ${result.ambiguous.length} process it could not prove belongs to this session.`,
              );
            }
            await Promise.allSettled([
              loadSessions(stateRef.current.scope, true),
              loadProjects(stateRef.current.scope),
              loadStatus(),
            ]);
          } catch (err) {
            notice("stop", asEngineError(err, session.key.providerId).message);
          }
        })();
      },
      closeConsole(id) {
        // The ONE place `console_close` is called from: an explicit request by the owner.
        //
        // **It is not idempotent.** An id the host no longer holds answers `NOT_FOUND`, and a
        // closed console is REMOVED from the registry rather than marked closed — so the id
        // vanishing from `console_list` is the closed signal, and a second close of the same id is
        // an error rather than a no-op. Two guards follow from that: the call only goes out for an
        // id the store still lists, and a rejection becomes a visible notice instead of being
        // swallowed, because a genuine NOT_FOUND here means the store and the host have drifted and
        // that is worth seeing.
        void (async () => {
          if (!stateRef.current.consoles[id]) {
            dispatch({ type: "console/visible", id: null });
            await loadConsoles();
            return;
          }
          try {
            await api.consoleClose({ id });
          } catch (err) {
            notice("console-close", asEngineError(err).message);
          }
          freshConsoles.current.delete(id);
          dispatch({ type: "console/visible", id: null });
          await loadConsoles();
        })();
      },
      refreshMetrics(key) {
        void (async () => {
          try {
            const metrics = await api.sessionMetrics({ key });
            dispatch({ type: "metrics/merge", rows: [{ key, metrics }] });
          } catch (err) {
            notice("metrics", asEngineError(err, key.providerId).message);
          }
        })();
      },
      refreshStatus() {
        void loadStatus();
      },
      toggleHover() {
        void (async () => {
          try {
            const { visible } = await api.hoverToggle();
            dispatch({ type: "hover/visible", visible });
          } catch {
            notice("hover", "The hover surface did not respond.");
          }
        })();
      },
      setHoverCompact(compact) {
        dispatch({ type: "hover/compact", compact });
      },
      async pickProject() {
        try {
          const picked = await api.projectPick();
          return picked?.cwd ?? null;
        } catch {
          notice("pick", "The folder picker did not open.");
          return null;
        }
      },
      openProject(cwd) {
        // Project discovery creates the projection; the View never writes a project row itself.
        void api.folderOpen({ cwd }).catch(() => notice("folder", `Pigeon could not open ${cwd}.`));
        void loadProjects(stateRef.current.scope);
      },
      dismissNotice(id) {
        dispatch({ type: "notice/dismiss", id });
      },
    };
  }, [api, loadSessions, loadProjects, loadStatus, loadConsoles]);

  // Whether the visible console has history to restore. Decided once per attach: a console this
  // pane opened a moment ago has nothing to replay, and every later attach to that same console
  // does — it has been running in the host the whole time the owner was looking at something else.
  if (replayRef.current.id !== state.visibleConsoleId) {
    replayRef.current = {
      id: state.visibleConsoleId,
      replay: state.visibleConsoleId ? !freshConsoles.current.has(state.visibleConsoleId) : false,
    };
  }
  const replayVisible = replayRef.current.replay;

  // "Fresh" expires when the panel attaches, which is the moment the console becomes visible.
  // Deleting it only on close — which is what this did — left `replayVisible` false for the life
  // of the console: leaving a session and coming back replayed nothing and showed an empty grid
  // where two hundred lines of output had been.
  useEffect(() => {
    if (state.visibleConsoleId) freshConsoles.current.delete(state.visibleConsoleId);
  }, [state.visibleConsoleId]);

  return { state, actions, replayVisible };
}

/** Exported for the tests that drive the reducer directly. */
export type { Action };

/**
 * Is the selected session absent from the scope currently on screen?
 *
 * Deliberately `inCurrentScope` and not `findSession`: the pane goes on RENDERING the row it has
 * (which `findSession` finds in either slice, so the numbers do not blank out), while this says
 * whether that row belongs to what the owner is looking at. Switching to Recent with a live session
 * selected has to say so — a live session's metrics sitting under a "Recent" heading with no notice
 * is the pane quietly claiming the session is closed.
 */
export function selectionIsStale(state: ViewState): boolean {
  if (state.selection.kind !== "session") return false;
  if (inCurrentScope(state, state.selection.key)) return false;
  // Only once the scope has actually answered — before that, "not found" means "not loaded".
  return state.sessions.generatedAtMs[state.scope] !== null && !state.sessions.loading[state.scope];
}

/** True when `key` is the selected session. Goes through `sameSession`, never a string compare of
 *  titles or a sid. */
export function isSelected(state: ViewState, key: SessionKey): boolean {
  return state.selection.kind === "session" && sameSession(state.selection.key, key);
}
