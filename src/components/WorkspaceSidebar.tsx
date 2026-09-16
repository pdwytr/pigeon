// The left column: scope, projects, and — in Recent — the flat seven-day session list.
//
// The flat list appears only in Recent, matching the mockup's `all-only` class. In Live the project
// cards already carry every live row, and a second copy of the same four rows underneath them would
// be noise pretending to be information.

import { useState } from "react";
import type {
  EngineError,
  MetricState,
  ProjectSummary,
  ProviderId,
  Scope,
  SessionKey,
  SessionRow,
} from "../bindings";
import { statusCountsForRows } from "../store/selectors";
import { NewProjectForm } from "./NewProjectForm";
import { ProjectList } from "./ProjectList";
import { panelId, ScopeTabs, tabId } from "./ScopeTabs";
import { SessionList } from "./SessionList";

export interface WorkspaceSidebarProps {
  scope: Scope;
  projects: ProjectSummary[];
  sessions: SessionRow[];
  /** The store's metric map. Rows read their numbers from here, never from the copy frozen into
   *  the list answer they arrived in. */
  metrics: Record<string, MetricState>;
  loading: boolean;
  /** Problems reading the scoped SESSION list. */
  problems: EngineError[] | null;
  /** Problems reading the scoped PROJECT summaries, which the project list renders in place of
   *  its empty state. */
  projectProblems: EngineError[] | null;
  selectedProject: string | null;
  selectedSession: SessionKey | null;
  expandedProjects: Set<string>;
  defaultProjectPath: string;
  onScopeChange(scope: Scope): void;
  onSelectProject(project: string): void;
  onToggleExpanded(project: string): void;
  onSelectSession(key: SessionKey): void;
  onOpenFolder(cwd: string): void;
  onAddSession(project: ProjectSummary, provider: ProviderId): void;
  onOpenProject(cwd: string): void;
  onPickProject(): Promise<string | null>;
}

export function WorkspaceSidebar(props: WorkspaceSidebarProps) {
  const { scope, projects, sessions, loading, problems } = props;
  const [formOpen, setFormOpen] = useState(false);

  const counts = statusCountsForRows(sessions);
  const liveCount = scope === "live" ? sessions.length : counts.running + counts.needsYou;

  return (
    <aside className="sidebar" aria-label="Projects and sessions">
      <div className="eyebrow">
        Workspace · {scope === "live" ? "live activity" : "last 7 days"}
      </div>
      <div className="sidebar-head">
        <div className="title">Projects</div>
        <span className="sidebar-count">
          {scope === "live" ? `${liveCount} live` : `${sessions.length} closed`}
        </span>
      </div>

      <ScopeTabs value={scope} loading={loading} onChange={props.onScopeChange} />

      {/* One engine failing must never remove another engine's rows, so a problem is a notice
          beside the list rather than a replacement for it. */}
      {problems?.length ? (
        <div className="unavailable" role="status" data-testid="scope-problems">
          {problems.map((p) => (
            <div key={`${p.provider ?? "host"}-${p.kind}`}>{p.message}</div>
          ))}
        </div>
      ) : null}

      {formOpen && (
        <NewProjectForm
          initialPath={props.defaultProjectPath}
          busy={false}
          onCancel={() => setFormOpen(false)}
          onOpenProject={(cwd) => {
            setFormOpen(false);
            props.onOpenProject(cwd);
          }}
          onPick={props.onPickProject}
        />
      )}

      <div className="section-label">
        <span>{scope === "live" ? "Live projects" : "Projects"}</span>
        <button type="button" onClick={() => setFormOpen((v) => !v)} aria-expanded={formOpen}>
          + New project
        </button>
      </div>

      <div id={panelId(scope)} role="tabpanel" aria-labelledby={tabId(scope)} tabIndex={-1}>
        <ProjectList
          scope={scope}
          projects={projects}
          sessions={sessions}
          metrics={props.metrics}
          loading={loading}
          problems={props.projectProblems}
          selectedProject={props.selectedProject}
          selectedSession={props.selectedSession}
          expandedProjects={props.expandedProjects}
          onSelectProject={props.onSelectProject}
          onToggleExpanded={props.onToggleExpanded}
          onSelectSession={props.onSelectSession}
          onOpenFolder={props.onOpenFolder}
          onAddSession={props.onAddSession}
        />

        {scope === "recent" && (
          <>
            <div className="section-label">
              <span>Sessions</span>
              <span>7-day window</span>
            </div>
            <SessionList
              rows={sessions}
              metrics={props.metrics}
              selectedKey={props.selectedSession}
              variant="flat"
              showProject
              label="Sessions closed in the last 7 days"
              emptyText="No sessions closed in the last 7 days."
              onSelect={props.onSelectSession}
            />
          </>
        )}
      </div>
    </aside>
  );
}
