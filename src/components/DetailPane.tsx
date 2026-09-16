// The right pane. One location, three things in it, and never a second page.
//
// Project metrics, session metrics and the embedded terminal all occupy this same area. Selecting a
// session does not navigate; it changes what this pane is about, and the terminal is inserted BELOW
// the selected session's numbers rather than replacing them. That is the contract's central
// interaction claim (§5.7, §6) and the mockup's shape, and it is why there is no router in Pigeon.
//
// The pane also owns one honest refusal: when the selected session is not in the scope currently on
// screen, it says so and offers a way out, rather than silently clearing the selection or — worse —
// rendering the row's last-known status as if it were current.

import type { ConsoleApi } from "../api/types";
import type {
  AccountStatus,
  ConsoleSummary,
  LiveSessionState,
  MetricState,
  ProjectSummary,
  ProviderId,
  Scope,
  SessionKey,
  SessionRow,
} from "../bindings";
import { PROVIDER_LABELS } from "../bindings";
import type { TerminalFactory } from "../console/terminalEngine";
import { formatAgo, formatCount, formatTokens, shortenPath, totalTokens } from "../format";
import { statusCountsForRows } from "../store/selectors";
import type { Selection } from "../store/viewStore";
import { AccountsStrip } from "./CapacityMeter";
import { EngineMenu } from "./EngineMenu";
import { MetricsSection } from "./MetricsSection";
import { SessionList } from "./SessionList";
import { sessionLabel } from "./SessionListRow";
import { StatusSection } from "./StatusSection";
import { TerminalPanel } from "./TerminalPanel";

export interface DetailPaneProps {
  scope: Scope;
  selection: Selection;
  project: ProjectSummary | null;
  projectSessions: SessionRow[];
  /** The store's metric map, keyed by `sessionKeyId`. */
  metrics: Record<string, MetricState>;
  selectedSession: SessionRow | null;
  selectedMetrics: MetricState | null;
  liveStatus: LiveSessionState | null;
  accounts: Partial<Record<ProviderId, AccountStatus>>;
  consoles: Record<string, ConsoleSummary>;
  visibleConsole: ConsoleSummary | null;
  replayConsole: boolean;
  hostOs: string | null;
  consoleApi: ConsoleApi;
  terminalFactory?: TerminalFactory;
  /** The selected session is not in the scope on screen. */
  stale: boolean;
  onSelectSession(key: SessionKey): void;
  onOpenFolder(cwd: string): void;
  onAddSession(project: ProjectSummary, provider: ProviderId): void;
  onResume(session: SessionRow): void;
  onStop(session: SessionRow): void;
  onCloseConsole(consoleId: string): void;
  onRefreshMetrics(key: SessionKey): void;
  onConsoleActivity(): void;
  onClearSelection(): void;
  onSwitchScope(scope: Scope): void;
}

/** The rail's figures for a session, which come from the metric state and nowhere else. A pending
 *  or unavailable state produces a WORD, never a zero. */
function sessionRail(state: MetricState | null): { label: string; value: string }[] {
  if (!state || state.state === "pending") {
    return [
      { label: "tokens", value: "counting" },
      { label: "API calls", value: "counting" },
      { label: "tools", value: "counting" },
    ];
  }
  if (state.state === "unavailable") {
    return [
      { label: "tokens", value: "n/a" },
      { label: "API calls", value: "n/a" },
      { label: "tools", value: "n/a" },
    ];
  }
  const m = state.value;
  return [
    { label: "tokens", value: formatTokens(totalTokens(m)) },
    { label: "API calls", value: formatCount(m.apiCalls) },
    { label: "tools", value: formatCount(m.toolCalls) },
  ];
}

export function DetailPane(props: DetailPaneProps) {
  const { scope, selection, project, selectedSession } = props;

  if (selection.kind === "none" || (!project && !selectedSession)) {
    return (
      <main className="main" data-testid="detail-pane">
        <div className="eyebrow" data-testid="detail-eyebrow">
          Detail
        </div>
        <div className="detail-head">
          <div>
            <h1>Select a project</h1>
            <p className="project-note">
              Pick a project on the left to see its totals, or a session to see its own metrics and
              resume it here.
            </p>
          </div>
        </div>
      </main>
    );
  }

  if (selection.kind === "session" && selectedSession) {
    const label = sessionLabel(selectedSession);
    const statusWord =
      selectedSession.status === "needs_you" ? "unknown" : (selectedSession.status ?? "no status");
    const provable = props.liveStatus?.process === "present";
    const attachedHere = props.visibleConsole?.sessionKey
      ? props.visibleConsole.sessionKey.providerId === selectedSession.key.providerId &&
        props.visibleConsole.sessionKey.sid === selectedSession.key.sid
      : false;

    return (
      <main className="main" data-testid="detail-pane">
        <div className="eyebrow" data-testid="detail-eyebrow">
          Session metrics · {statusWord}
        </div>
        <div className="detail-head">
          <div>
            <h1>{label}</h1>
            <p className="project-note">
              {selectedSession.projectName} · {PROVIDER_LABELS[selectedSession.key.providerId]} ·{" "}
              {formatAgo(selectedSession.closedAtMs ?? selectedSession.lastActiveMs)}
            </p>
          </div>
          <div className="detail-actions">
            <button
              type="button"
              className="open-project"
              onClick={() => props.onOpenFolder(selectedSession.cwd ?? selectedSession.project)}
            >
              Open folder
            </button>
            {/* Stop is offered only for a session with a PROVEN live process. Without one, the
                host would have nothing to act on and the button would be a promise Pigeon
                cannot keep. */}
            {provable && (
              <button
                type="button"
                className="stop-session"
                onClick={() => props.onStop(selectedSession)}
              >
                Stop session
              </button>
            )}
            {attachedHere && props.visibleConsole ? (
              <button
                type="button"
                className="action"
                onClick={() => props.onCloseConsole(props.visibleConsole?.id ?? "")}
              >
                Close terminal
              </button>
            ) : (
              <button
                type="button"
                className="action"
                disabled={!selectedSession.resumable}
                title={selectedSession.resumeBlockedReason ?? undefined}
                onClick={() => props.onResume(selectedSession)}
              >
                Resume in terminal
              </button>
            )}
          </div>
        </div>

        {!selectedSession.resumable && selectedSession.resumeBlockedReason && (
          // The host already wrote this as a sentence. Rendered, never parsed.
          <p className="project-note" data-testid="resume-blocked">
            {selectedSession.resumeBlockedReason}
          </p>
        )}

        {props.stale && (
          <div
            className="unavailable"
            role="status"
            data-testid="selection-stale"
            style={{ marginTop: 12 }}
          >
            No longer in this view.{" "}
            <button
              type="button"
              className="icon-btn"
              onClick={() => props.onSwitchScope(scope === "live" ? "recent" : "live")}
            >
              Look in {scope === "live" ? "Recent" : "Live"}
            </button>{" "}
            <button type="button" className="icon-btn" onClick={props.onClearSelection}>
              Clear selection
            </button>
          </div>
        )}

        <div className="summary-rail" style={{ margin: "22px 0 0" }}>
          <span>
            <strong className={selectedSession.status ?? "unknown"}>{statusWord}</strong>
            <span className="rail-label">status</span>
          </span>
          {sessionRail(props.selectedMetrics).map((cell) => (
            <span key={cell.label}>
              <strong>{cell.value}</strong>
              <span className="rail-label">{cell.label}</span>
            </span>
          ))}
        </div>

        <StatusSection row={selectedSession} observation={props.liveStatus} />

        <MetricsSection
          scope="session"
          state={props.selectedMetrics}
          onRetry={() => props.onRefreshMetrics(selectedSession.key)}
        />

        {props.visibleConsole && attachedHere && (
          <TerminalPanel
            summary={props.visibleConsole}
            api={props.consoleApi}
            terminalFactory={props.terminalFactory}
            hostOs={props.hostOs}
            replay={props.replayConsole}
            label={`${PROVIDER_LABELS[props.visibleConsole.provider]} terminal for ${label} in ${
              selectedSession.projectName
            }`}
            onClose={props.onCloseConsole}
            onActivity={props.onConsoleActivity}
          />
        )}
      </main>
    );
  }

  if (!project) return null;

  const projectStatusCounts = statusCountsForRows(props.projectSessions);
  const liveCount =
    scope === "live"
      ? props.projectSessions.length
      : projectStatusCounts.running + projectStatusCounts.needsYou;
  // A visible console with no session of its own, started in this project's folder.
  const projectConsole =
    props.visibleConsole &&
    props.visibleConsole.sessionKey === null &&
    props.visibleConsole.cwd === project.cwd
      ? props.visibleConsole
      : null;
  return (
    <main className="main" data-testid="detail-pane">
      <div className="eyebrow" data-testid="detail-eyebrow">
        Project metrics · {scope === "live" ? "live activity" : "last 7 days"}
      </div>
      <div className="detail-head">
        <div>
          <h1>{project.projectName}</h1>
          <p className="project-note" title={project.cwd}>
            {shortenPath(project.cwd)} · {project.sessions}{" "}
            {project.sessions === 1 ? "session" : "sessions"}
            {scope === "live" ? ` · ${liveCount} live` : ""}
          </p>
        </div>
        <div className="detail-actions">
          <button
            type="button"
            className="open-project"
            onClick={() => props.onOpenFolder(project.cwd)}
          >
            Open folder
          </button>
          <EngineMenu
            className="action"
            onChoose={(provider) => props.onAddSession(project, provider)}
          />
        </div>
      </div>

      <div className="summary-rail" style={{ margin: "22px 0 0" }}>
        <span>
          <strong className="running">{projectStatusCounts.running}</strong>
          <span className="rail-label">running</span>
        </span>
        <span>
          <strong className="finished">{projectStatusCounts.finished}</strong>
          <span className="rail-label">waiting</span>
        </span>
        <span>
          <strong>{formatTokens(totalTokens(project.totals))}</strong>
          <span className="rail-label">tokens</span>
        </span>
      </div>

      <section className="section" style={{ marginTop: 14 }}>
        <div className="row spread">
          <h3>{scope === "live" ? "Live sessions" : "Recent sessions"}</h3>
          <span className="subtle">
            {props.projectSessions.length}{" "}
            {props.projectSessions.length === 1 ? "session" : "sessions"} in this project
          </span>
        </div>
        <SessionList
          rows={props.projectSessions}
          metrics={props.metrics}
          selectedKey={null}
          consoles={props.consoles}
          variant="detail"
          showProject={false}
          label={`${project.projectName} sessions`}
          emptyText="No sessions in this view."
          onSelect={props.onSelectSession}
        />
      </section>

      <MetricsSection
        scope="project"
        totals={project.totals}
        kpis={project.kpis}
        coverage={{ counted: project.counted, sessions: project.sessions }}
        costRows={project.costRows}
      />

      <AccountsStrip accounts={props.accounts} />

      {/* A console started by "Add session" belongs to the PROJECT, not to a session: the engine
          has not written a discoverable record yet, so `session_start` returns a console whose
          `sessionKey` is null and there is no row to select. Rendering a terminal only in the
          session branch left a real PTY running in a terminal nobody could see or close, for the
          life of the app. */}
      {projectConsole && (
        <TerminalPanel
          summary={projectConsole}
          api={props.consoleApi}
          terminalFactory={props.terminalFactory}
          hostOs={props.hostOs}
          replay={props.replayConsole}
          label={`${PROVIDER_LABELS[projectConsole.provider]} terminal in ${project.projectName}`}
          onClose={props.onCloseConsole}
          onActivity={props.onConsoleActivity}
        />
      )}

      <section className="section">
        <h3>Session actions</h3>
        <p className="subtle">
          Select a session above to see its metrics and resume it in this pane. Adding a session
          opens the chosen engine in this project folder.
        </p>
      </section>
    </main>
  );
}
