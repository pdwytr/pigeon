// The normalized view state of `docs/contracts/components.md` §3, and the reducer over it.
//
// A reducer rather than a scatter of `useState` calls, for one reason above all others: **the
// stale-response rule is a property of the store, not of a component.** Each scope's data can only
// be replaced by the newest request for that scope, and the only way to say that once — instead of
// once per caller, forgotten in at least one of them — is to make the comparison part of the
// transition itself. `sessions/loaded` and `projects/loaded` carry the token they were issued with,
// and a token that is not the current one for that scope returns the state unchanged.
//
// Everything here is a pure function of (state, action). No fetching, no timers, no api. Those live
// in `usePigeonApp`, which is what makes the interesting rules testable without a render.

import type {
  AccountStatus,
  ConsoleSummary,
  EngineError,
  MetricState,
  ProjectSummary,
  ProjectSummaryResult,
  ProviderId,
  Scope,
  SessionKey,
  SessionListResult,
  SessionRow,
  Settings,
  StatusSnapshot,
} from "../bindings";
import { sessionKeyId } from "../bindings";

export type Selection =
  | { kind: "project"; project: string }
  | { kind: "session"; key: SessionKey }
  | { kind: "none" };

export interface ConsoleUiState {
  attached: boolean;
  rendererReady: boolean;
  replaying: boolean;
  fit: { cols: number; rows: number } | null;
  localScrollTop: number;
  inputEnabled: boolean;
}

export interface Notice {
  id: string;
  tone: "info" | "error";
  text: string;
}

type ByScope<T> = Record<Scope, T>;

export interface ScopedSlice<T> {
  live: T[];
  recent: T[];
  loading: ByScope<boolean>;
  error: ByScope<EngineError[] | null>;
  generatedAtMs: ByScope<number | null>;
}

export interface ViewState {
  scope: Scope;
  selection: Selection;
  expandedProjects: Set<string>;
  /** Every project key a live answer has ever named. Expansion is a DEFAULT for a card the owner
   *  has not met yet, not a state re-asserted under them on every refresh. */
  seenProjects: Set<string>;
  sessions: ScopedSlice<SessionRow>;
  projects: ScopedSlice<ProjectSummary>;
  /** Keyed by `sessionKeyId(key)`. Never by `sid`, never by title. */
  metrics: Record<string, MetricState>;
  status: StatusSnapshot | null;
  accounts: Partial<Record<ProviderId, AccountStatus>>;
  settings: Settings | null;
  hostOs: string | null;
  consoles: Record<string, ConsoleSummary>;
  visibleConsoleId: string | null;
  consoleUi: Record<string, ConsoleUiState>;
  hoverVisible: boolean;
  hoverCompact: boolean;
  lastUpdatedMs: number | null;
  refreshing: boolean;
  notices: Notice[];
  /** The newest issued request token per scope, per resource. See the header. `consoles` is not
   *  scoped — there is one console registry — but a late `console_list` is exactly as dangerous as
   *  a late session list: it restores a console the owner has closed. */
  tokens: { sessions: ByScope<number>; projects: ByScope<number>; consoles: number };
}

function emptySlice<T>(): ScopedSlice<T> {
  return {
    live: [],
    recent: [],
    loading: { live: false, recent: false },
    error: { live: null, recent: null },
    generatedAtMs: { live: null, recent: null },
  };
}

export function initialState(scope: Scope = "live"): ViewState {
  return {
    scope,
    selection: { kind: "none" },
    expandedProjects: new Set(),
    seenProjects: new Set(),
    sessions: emptySlice<SessionRow>(),
    projects: emptySlice<ProjectSummary>(),
    metrics: {},
    status: null,
    accounts: {},
    settings: null,
    hostOs: null,
    consoles: {},
    visibleConsoleId: null,
    consoleUi: {},
    hoverVisible: false,
    hoverCompact: false,
    lastUpdatedMs: null,
    refreshing: false,
    notices: [],
    tokens: {
      sessions: { live: 0, recent: 0 },
      projects: { live: 0, recent: 0 },
      consoles: 0,
    },
  };
}

export type Action =
  | { type: "host/info"; os: string }
  | { type: "settings/set"; settings: Settings }
  | { type: "scope/set"; scope: Scope }
  | { type: "select/project"; project: string }
  | { type: "select/session"; key: SessionKey }
  | { type: "select/clear" }
  | { type: "project/toggle"; project: string }
  | { type: "project/expand"; projects: string[] }
  | { type: "sessions/request"; scope: Scope; token: number }
  | { type: "sessions/loaded"; token: number; result: SessionListResult }
  | { type: "sessions/failed"; scope: Scope; token: number; error: EngineError }
  | { type: "projects/request"; scope: Scope; token: number }
  | { type: "projects/loaded"; token: number; result: ProjectSummaryResult }
  | { type: "projects/failed"; scope: Scope; token: number; error: EngineError }
  | { type: "metrics/merge"; rows: { key: SessionKey; metrics: MetricState }[] }
  | { type: "status/set"; snapshot: StatusSnapshot }
  | { type: "accounts/set"; accounts: Partial<Record<ProviderId, AccountStatus>> }
  | { type: "consoles/set"; token: number; consoles: ConsoleSummary[] }
  | { type: "console/visible"; id: string | null }
  | { type: "console/ui"; id: string; patch: Partial<ConsoleUiState> }
  | { type: "console/exited"; id: string; exitCode: number | null }
  | { type: "refresh/start" }
  | { type: "refresh/end"; atMs: number }
  | { type: "hover/visible"; visible: boolean }
  | { type: "hover/compact"; compact: boolean }
  | { type: "notice/push"; notice: Notice }
  | { type: "notice/dismiss"; id: string };

const DEFAULT_CONSOLE_UI: ConsoleUiState = {
  attached: false,
  rendererReady: false,
  replaying: false,
  fit: null,
  localScrollTop: 0,
  inputEnabled: false,
};

/** Copy a slice so the per-scope maps inside it can be mutated locally without touching the
 *  previous state object. The caller then writes only the scope it owns. */
function copySlice<T>(slice: ScopedSlice<T>, patch: Partial<ScopedSlice<T>> = {}): ScopedSlice<T> {
  return {
    ...slice,
    ...patch,
    loading: { ...slice.loading },
    error: { ...slice.error },
    generatedAtMs: { ...slice.generatedAtMs },
  };
}

export function reducer(state: ViewState, action: Action): ViewState {
  switch (action.type) {
    case "host/info":
      return { ...state, hostOs: action.os };

    case "settings/set":
      return {
        ...state,
        settings: action.settings,
        // The host's answer is the authority on whether the hover WINDOW is up. There is no
        // visibility event in the wire contract, so this action is also how a hover that closed
        // itself reaches the dashboard's button (`usePigeonApp` re-reads settings on focus).
        hoverVisible: action.settings.hover.visible,
        // The remembered tab, adopted ONCE: on the first settings answer, before the owner has
        // touched anything. Re-adopting it on a later read — the focus re-read, say — would move
        // the tab under someone who had just changed it.
        scope: state.settings === null ? action.settings.list.view : state.scope,
      };

    case "scope/set":
      if (action.scope === state.scope) return state;
      // Selection survives a scope change by contract (§3.1: "Selection changes do not change the
      // API scope", and the pane says "no longer in this view" rather than silently clearing).
      return { ...state, scope: action.scope };

    case "select/project":
      return { ...state, selection: { kind: "project", project: action.project } };

    case "select/session":
      return { ...state, selection: { kind: "session", key: action.key } };

    case "select/clear":
      return { ...state, selection: { kind: "none" } };

    case "project/toggle": {
      const next = new Set(state.expandedProjects);
      if (next.has(action.project)) next.delete(action.project);
      else next.add(action.project);
      return { ...state, expandedProjects: next };
    }

    case "project/expand": {
      const next = new Set(state.expandedProjects);
      for (const p of action.projects) next.add(p);
      return { ...state, expandedProjects: next };
    }

    case "sessions/request": {
      const sessions = copySlice(state.sessions);
      sessions.loading[action.scope] = true;
      return {
        ...state,
        sessions,
        tokens: {
          ...state.tokens,
          sessions: { ...state.tokens.sessions, [action.scope]: action.token },
        },
      };
    }

    case "sessions/loaded": {
      const { scope } = action.result;
      // The whole reason this reducer exists. A late answer for a scope that has since been asked
      // again is DROPPED, not merged: merging it would resurrect rows the newer answer removed.
      if (action.token !== state.tokens.sessions[scope]) return state;
      const sessions = copySlice(state.sessions, { [scope]: action.result.rows });
      sessions.loading[scope] = false;
      sessions.error[scope] = action.result.problems.length ? action.result.problems : null;
      sessions.generatedAtMs[scope] = action.result.generatedAtMs;
      // Rows carry their own metric state; fold it into the one map every surface reads, so a
      // session's numbers do not depend on which list it was rendered from.
      const metrics = { ...state.metrics };
      for (const row of action.result.rows) metrics[sessionKeyId(row.key)] = row.metrics;
      return { ...state, sessions, metrics, lastUpdatedMs: action.result.generatedAtMs };
    }

    case "sessions/failed": {
      if (action.token !== state.tokens.sessions[action.scope]) return state;
      const sessions = copySlice(state.sessions);
      sessions.loading[action.scope] = false;
      sessions.error[action.scope] = [action.error];
      return { ...state, sessions };
    }

    case "projects/request": {
      const projects = copySlice(state.projects);
      projects.loading[action.scope] = true;
      return {
        ...state,
        projects,
        tokens: {
          ...state.tokens,
          projects: { ...state.tokens.projects, [action.scope]: action.token },
        },
      };
    }

    case "projects/loaded": {
      const { scope } = action.result;
      if (action.token !== state.tokens.projects[scope]) return state;
      const projects = copySlice(state.projects, { [scope]: action.result.projects });
      projects.loading[scope] = false;
      projects.error[scope] = null;
      projects.generatedAtMs[scope] = action.result.generatedAtMs;
      // Live cards default expanded, matching the mockup; Recent keeps whatever the user chose.
      //
      // "Default", and only that. Unioning every key on every answer re-asserted the default
      // forever, so a collapsed card sprang open on the next refresh five seconds later. Only a
      // project this store has never seen in a live answer is expanded for the owner.
      let expandedProjects = state.expandedProjects;
      let seenProjects = state.seenProjects;
      if (scope === "live") {
        const unseen = action.result.projects
          .map((p) => p.project)
          .filter((key) => !state.seenProjects.has(key));
        if (unseen.length) {
          expandedProjects = new Set([...state.expandedProjects, ...unseen]);
          seenProjects = new Set([...state.seenProjects, ...unseen]);
        }
      }
      return { ...state, projects, expandedProjects, seenProjects };
    }

    case "projects/failed": {
      if (action.token !== state.tokens.projects[action.scope]) return state;
      const projects = copySlice(state.projects);
      projects.loading[action.scope] = false;
      projects.error[action.scope] = [action.error];
      return { ...state, projects };
    }

    case "metrics/merge": {
      const metrics = { ...state.metrics };
      for (const row of action.rows) metrics[sessionKeyId(row.key)] = row.metrics;
      return { ...state, metrics };
    }

    case "status/set":
      // Replaced atomically. A snapshot is the host's complete answer about what is live; merging
      // it into the previous one would leave a stopped session looking live forever.
      //
      // `status_snapshot` carries no request token, and two are in flight whenever a poll overlaps
      // a Stop. The host's own `generatedAtMs` is the only ordering there is, so an answer older
      // than the one on screen is dropped — otherwise a session the owner just stopped comes back
      // to life because the read that saw it running finished last.
      if (state.status && action.snapshot.generatedAtMs < state.status.generatedAtMs) return state;
      return { ...state, status: action.snapshot };

    case "accounts/set":
      return { ...state, accounts: action.accounts };

    case "consoles/set": {
      // Same rule as the scoped loaders, for the same reason: a late `console_list` restores a
      // console that has since been closed, and selecting it attaches to a dead id.
      if (action.token < state.tokens.consoles) return state;
      const consoles: Record<string, ConsoleSummary> = {};
      for (const c of action.consoles) consoles[c.id] = c;
      // A console that the host no longer lists takes its UI state with it, and stops being the
      // visible one — otherwise the pane would hold an id nothing can answer for.
      const consoleUi: Record<string, ConsoleUiState> = {};
      for (const id of Object.keys(state.consoleUi)) {
        if (consoles[id]) consoleUi[id] = state.consoleUi[id];
      }
      const visibleConsoleId =
        state.visibleConsoleId && consoles[state.visibleConsoleId] ? state.visibleConsoleId : null;
      return {
        ...state,
        consoles,
        consoleUi,
        visibleConsoleId,
        tokens: { ...state.tokens, consoles: action.token },
      };
    }

    case "console/visible":
      return { ...state, visibleConsoleId: action.id };

    case "console/ui": {
      const current = state.consoleUi[action.id] ?? DEFAULT_CONSOLE_UI;
      return {
        ...state,
        consoleUi: { ...state.consoleUi, [action.id]: { ...current, ...action.patch } },
      };
    }

    case "console/exited": {
      const summary = state.consoles[action.id];
      if (!summary) return state;
      return {
        ...state,
        consoles: {
          ...state.consoles,
          [action.id]: { ...summary, state: "exited", exitCode: action.exitCode },
        },
      };
    }

    case "refresh/start":
      return { ...state, refreshing: true };

    case "refresh/end":
      return { ...state, refreshing: false, lastUpdatedMs: action.atMs };

    case "hover/visible":
      return { ...state, hoverVisible: action.visible };

    case "hover/compact":
      return { ...state, hoverCompact: action.compact };

    case "notice/push":
      // Bounded: a host that fails every poll must not grow an unbounded list of identical notices.
      return {
        ...state,
        notices: [...state.notices.filter((n) => n.id !== action.notice.id), action.notice].slice(
          -4,
        ),
      };

    case "notice/dismiss":
      return { ...state, notices: state.notices.filter((n) => n.id !== action.id) };

    default:
      return state;
  }
}
