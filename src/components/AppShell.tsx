// The shell: one store, one api, four regions.
//
// It owns nothing a component could own for itself. Its jobs are to hold the store, to turn the
// store's shape into the props each region declares, and to apply the two selection rules that are
// genuinely global:
//
//   * with nothing selected, the pane opens on the first project in the current scope, so the app
//     never boots onto an empty right-hand side;
//   * a PROJECT selection that does not survive a scope change falls back to the first project in
//     the new scope. A SESSION selection does not: it stays selected and the pane says "no longer
//     in this view", because silently dropping the thing the owner was looking at is worse than
//     telling them where it went.

import { useEffect, useMemo } from "react";
import type { FeatherApi } from "../api/types";
import type { TerminalFactory } from "../console/terminalEngine";
import {
  findProject,
  findSession,
  liveStateFor,
  metricsFor,
  selectedKey,
  selectedProjectKey,
  sessionsForProject,
  visibleProjects,
  visibleSessions,
} from "../store/selectors";
import { selectionIsStale, useFeatherApp } from "../store/useFeatherApp";
import { DetailPane } from "./DetailPane";
import { Topbar } from "./Topbar";
import { WorkspaceSidebar } from "./WorkspaceSidebar";

export interface AppShellProps {
  api: FeatherApi;
  /** Injected in tests; production builds a real xterm inside `ConsoleView`. */
  terminalFactory?: TerminalFactory;
  /** Poll the status snapshot on this interval. Zero (the default, and what tests use) relies on
   *  `status://changed` alone, so no test owns a timer it did not ask for. */
  pollMs?: number;
}

/** Where the "new project" field starts: beside whatever project is already open, or nothing. */
function parentOf(cwd: string | undefined): string {
  if (!cwd) return "";
  const idx = Math.max(cwd.lastIndexOf("/"), cwd.lastIndexOf("\\"));
  return idx > 0 ? cwd.slice(0, idx + 1) : cwd;
}

export function AppShell({ api, terminalFactory, pollMs = 0 }: AppShellProps) {
  const { state, actions, replayVisible } = useFeatherApp(api, pollMs);

  const sessions = visibleSessions(state);
  const projects = visibleProjects(state);
  const projectKey = selectedProjectKey(state);
  const project = findProject(state, projectKey);
  const key = selectedKey(state);
  const selectedSession = findSession(state, key);
  const projectsLoaded = state.projects.generatedAtMs[state.scope] !== null;

  // Selection fallback. Runs only once the scope has actually answered, so "no project matches" is
  // never confused with "no project has arrived yet".
  useEffect(() => {
    if (!projectsLoaded || projects.length === 0) return;
    const selection = state.selection;
    if (selection.kind === "none") {
      actions.selectProject(projects[0].project);
      return;
    }
    // A project that did not survive the scope change hands the pane to the first one that did.
    if (selection.kind === "project" && !projects.some((p) => p.project === selection.project)) {
      actions.selectProject(projects[0].project);
    }
  }, [projectsLoaded, projects, state.selection, actions]);

  const projectSessions = useMemo(
    () => (projectKey ? sessionsForProject(sessions, projectKey) : []),
    [sessions, projectKey],
  );

  const visibleConsole = state.visibleConsoleId
    ? (state.consoles[state.visibleConsoleId] ?? null)
    : null;
  const defaultProjectPath = parentOf(projects[0]?.cwd);

  return (
    <div className="app">
      <Topbar
        lastUpdatedMs={state.lastUpdatedMs}
        refreshing={state.refreshing}
        hoverVisible={state.hoverVisible}
        onRefresh={actions.refresh}
        onToggleHover={actions.toggleHover}
      />

      {state.notices.length > 0 && (
        <div style={{ padding: "10px 22px 0" }} data-testid="notices">
          {state.notices.map((n) => (
            <div className="notice" role="status" key={n.id} style={{ marginBottom: 6 }}>
              <div className="row spread">
                <span>{n.text}</span>
                <button
                  type="button"
                  className="icon-btn"
                  onClick={() => actions.dismissNotice(n.id)}
                >
                  Dismiss
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      <div className="content">
        <WorkspaceSidebar
          scope={state.scope}
          projects={projects}
          sessions={sessions}
          metrics={state.metrics}
          loading={state.sessions.loading[state.scope] || state.projects.loading[state.scope]}
          problems={state.sessions.error[state.scope]}
          projectProblems={state.projects.error[state.scope]}
          selectedProject={projectKey}
          selectedSession={key}
          expandedProjects={state.expandedProjects}
          defaultProjectPath={defaultProjectPath}
          onScopeChange={actions.setScope}
          onSelectProject={actions.selectProject}
          onToggleExpanded={actions.toggleProject}
          onSelectSession={actions.selectSession}
          onOpenFolder={actions.openFolder}
          onAddSession={actions.addSession}
          onOpenProject={actions.openProject}
          onPickProject={actions.pickProject}
        />

        <DetailPane
          scope={state.scope}
          selection={state.selection}
          project={project}
          projectSessions={projectSessions}
          metrics={state.metrics}
          selectedSession={selectedSession}
          selectedMetrics={metricsFor(state, key)}
          liveStatus={liveStateFor(state, key)}
          accounts={state.accounts}
          consoles={state.consoles}
          visibleConsole={visibleConsole}
          replayConsole={replayVisible}
          hostOs={state.hostOs}
          consoleApi={api}
          terminalFactory={terminalFactory}
          stale={selectionIsStale(state)}
          onSelectSession={actions.selectSession}
          onOpenFolder={actions.openFolder}
          onAddSession={actions.addSession}
          onResume={actions.resume}
          onStop={actions.stop}
          onCloseConsole={actions.closeConsole}
          onRefreshMetrics={actions.refreshMetrics}
          onConsoleActivity={actions.refreshStatus}
          onClearSelection={actions.clearSelection}
          onSwitchScope={actions.setScope}
        />
      </div>

      {/* The hover is a SEPARATE always-on-top window (`tauri.conf.json`'s `hover` label,
          rendered by `HoverSurface`), not an overlay in this one. Rendering it here as well gave
          the same button two hovers: a panel over the dashboard AND a floating window. The owner
          asked for a surface that stays visible while they work in another app, which only the
          real window can be. */}
    </div>
  );
}
