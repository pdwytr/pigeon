// One project, with its scoped sessions folded underneath it.
//
// Two facts the card is careful about, both from the contract:
//
//   * **The status dot comes from `statusCounts`, not from whether Pigeon has a console attached.**
//     A session running in the owner's own terminal is exactly as live as one Pigeon launched, and
//     a dot that only lit for Pigeon's own consoles would quietly redefine "live" as "ours".
//
//   * **In Recent scope the card represents CLOSED sessions and never shows them as live.** The
//     host already excludes live rows from the recent answer; the card additionally refuses to draw
//     the live dot outside the live scope, so a stale answer cannot paint one either.
//
// The header is a `<button>` and the chevron is a separate sibling button. The mockup makes the
// whole `<article>` clickable, which would mean either a div with an onClick (unreachable by
// keyboard) or a button inside a button (invalid). Two controls is the honest shape.

import type {
  MetricState,
  ProjectSummary,
  ProviderId,
  Scope,
  SessionKey,
  SessionRow,
  StatusCounts,
} from "../bindings";
import { formatTokens, shortenPath, totalTokens } from "../format";
import { EngineMenu } from "./EngineMenu";
import { SessionList } from "./SessionList";

export interface ProjectCardProps {
  project: ProjectSummary;
  statusCounts?: StatusCounts;
  scope: Scope;
  sessions: SessionRow[];
  /** The store's metric map, so a row's numbers come from the same place everywhere. */
  metrics?: Record<string, MetricState>;
  selected: boolean;
  selectedSession: SessionKey | null;
  expanded: boolean;
  onSelect(project: string): void;
  onToggleExpanded(project: string): void;
  onSelectSession(key: SessionKey): void;
  onOpenFolder(cwd: string): void;
  onAddSession(project: ProjectSummary, provider: ProviderId): void;
}

export function ProjectCard(props: ProjectCardProps) {
  const {
    project,
    statusCounts = project.statusCounts,
    scope,
    sessions,
    metrics,
    selected,
    selectedSession,
    expanded,
    onSelect,
    onToggleExpanded,
    onSelectSession,
    onOpenFolder,
    onAddSession,
  } = props;

  const liveCount =
    scope === "live" ? sessions.length : statusCounts.running + statusCounts.needsYou;
  const showLive = scope === "live" && liveCount > 0;
  const tokens = totalTokens(project.totals);

  return (
    <article
      className={selected ? "project-card selected" : "project-card"}
      data-testid={`project-card-${project.project}`}
    >
      <div className="project-top">
        <button
          type="button"
          className="project-expand"
          aria-expanded={expanded}
          aria-label={`${expanded ? "Hide" : "Show"} ${project.projectName} sessions`}
          onClick={() => onToggleExpanded(project.project)}
        >
          <span className="chevron" aria-hidden="true">
            ▾
          </span>
        </button>
        <div className="project-icon" aria-hidden="true">
          ⌘
        </div>
        <button
          type="button"
          className="project-head"
          aria-current={selected ? "true" : undefined}
          onClick={() => onSelect(project.project)}
          title={project.cwd}
        >
          <div className="project-name">{project.projectName}</div>
          {/* Shortened for the column width; the full path is the title and the label. */}
          <div className="project-path">{shortenPath(project.cwd)}</div>
        </button>
        {showLive && (
          <span className="project-status" data-testid={`project-live-${project.project}`}>
            <i className="dot" aria-hidden="true" />
            {liveCount} live
          </span>
        )}
      </div>

      <div className="project-stats">
        {scope === "live" ? (
          <span>
            <strong>{liveCount}</strong> live
          </span>
        ) : (
          <span>
            <strong>{statusCounts.finished}</strong> closed
          </span>
        )}
        <span aria-hidden="true">·</span>
        {/* Partial coverage says so rather than presenting a partial sum as a total. */}
        <span>
          <strong>{formatTokens(tokens)}</strong> tokens
          {project.counted < project.sessions ? " so far" : ""}
        </span>
        <span aria-hidden="true">·</span>
        <span>
          <strong>{project.sessions}</strong> {project.sessions === 1 ? "session" : "sessions"}
        </span>
      </div>

      {expanded && (
        <SessionList
          rows={sessions}
          metrics={metrics}
          selectedKey={selectedSession}
          variant="project"
          showProject={false}
          label={`${project.projectName} sessions`}
          emptyText="No sessions in this view."
          onSelect={onSelectSession}
        />
      )}

      <div className="project-actions">
        <button type="button" className="primary" onClick={() => onOpenFolder(project.cwd)}>
          Open project
        </button>
        <EngineMenu onChoose={(provider) => onAddSession(project, provider)} />
      </div>
    </article>
  );
}
