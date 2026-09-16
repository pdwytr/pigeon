// The scoped stack of project cards.
//
// A project with no rows in the selected scope is not rendered (§3.2). That is a rule about the
// LIST, not about the host's answer: the host already scopes its projects, and this is the second
// gate that keeps a project card from outliving the sessions that justified it.

import type {
  EngineError,
  MetricState,
  ProjectSummary,
  ProviderId,
  Scope,
  SessionKey,
  SessionRow,
} from "../bindings";
import { sessionsForProject, statusCountsForRows } from "../store/selectors";
import { ProjectCard } from "./ProjectCard";

export interface ProjectListProps {
  scope: Scope;
  projects: ProjectSummary[];
  sessions: SessionRow[];
  metrics?: Record<string, MetricState>;
  loading: boolean;
  /** What went wrong reading the project summaries for this scope, if anything. Written by the
   *  reducer and — until an audit found it — read by nothing, so a `projects_summary` the host
   *  could not answer rendered as the confident sentence "No live projects". */
  problems: EngineError[] | null;
  selectedProject: string | null;
  selectedSession: SessionKey | null;
  expandedProjects: Set<string>;
  onSelectProject(project: string): void;
  onToggleExpanded(project: string): void;
  onSelectSession(key: SessionKey): void;
  onOpenFolder(cwd: string): void;
  onAddSession(project: ProjectSummary, provider: ProviderId): void;
}

function Problem({ problems }: { problems: EngineError[] }) {
  return (
    <div className="unavailable" role="status" data-testid="project-list-problem">
      {problems.map((p) => (
        <div key={`${p.provider ?? "host"}-${p.kind}-${p.message}`}>{p.message}</div>
      ))}
    </div>
  );
}

export function ProjectList(props: ProjectListProps) {
  const { scope, projects, sessions, loading, problems } = props;

  if (loading && projects.length === 0) {
    return (
      <div className="project-stack" data-testid="project-skeletons" aria-busy="true">
        <div className="skeleton" />
        <div className="skeleton" />
        <div className="skeleton" />
        <span className="sr-only">Loading projects</span>
      </div>
    );
  }

  const withRows = projects.filter((p) => sessions.some((s) => s.project === p.project));

  // A failure is not an absence. "No live projects" is a claim about the machine; a
  // `projects_summary` that never answered says nothing about what is running on it.
  if (withRows.length === 0 && problems?.length) {
    return <Problem problems={problems} />;
  }

  if (withRows.length === 0) {
    return (
      <p className="empty-live" data-testid="project-list-empty">
        {scope === "live"
          ? "No live projects. Nothing is running in Claude Code, Codex or OpenCode right now."
          : "No sessions closed in the last 7 days."}
      </p>
    );
  }

  return (
    <div className="project-stack" data-testid="project-list">
      {/* Partial failure keeps the projects that did arrive and says what did not (§8). */}
      {problems?.length ? <Problem problems={problems} /> : null}
      {withRows.map((project) => (
        <ProjectCard
          key={project.project}
          project={project}
          statusCounts={statusCountsForRows(sessionsForProject(sessions, project.project))}
          scope={scope}
          sessions={sessionsForProject(sessions, project.project)}
          metrics={props.metrics}
          selected={props.selectedProject === project.project}
          selectedSession={props.selectedSession}
          expanded={props.expandedProjects.has(project.project)}
          onSelect={props.onSelectProject}
          onToggleExpanded={props.onToggleExpanded}
          onSelectSession={props.onSelectSession}
          onOpenFolder={props.onOpenFolder}
          onAddSession={props.onAddSession}
        />
      ))}
    </div>
  );
}
