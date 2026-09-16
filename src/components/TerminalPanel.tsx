// The chrome around a console, in the SAME pane as the session's metrics.
//
// The whole design of this panel is "no second place to be". Resume does not open a tab, a route or
// a window; it puts a terminal below the numbers that describe the session it belongs to, so the
// metrics and the live output are readable together. `DetailPane` renders it last, under the KPIs.
//
// `Close terminal` is the only control here that ends a process. Switching to another session
// unmounts this panel and the host keeps the PTY running — see `ConsoleView`'s cleanup, which
// unsubscribes and disposes the renderer and calls nothing.

import type { ConsoleApi } from "../api/types";
import type { ConsoleSummary } from "../bindings";
import { PROVIDER_LABELS } from "../bindings";
import { ConsoleView, type ConsoleViewState } from "../console/ConsoleView";
import type { TerminalFactory } from "../console/terminalEngine";
import { shortenPath } from "../format";

const STATE_WORDS: Record<ConsoleSummary["state"], string> = {
  starting: "starting",
  // This is the PTY lifecycle, not the agent's turn state. An open terminal may be idle at a
  // prompt, so calling it "process running" makes the activity badge appear to lie.
  running: "terminal connected",
  exited: "process ended",
  closed: "closed",
};

export interface TerminalPanelProps {
  summary: ConsoleSummary;
  api: ConsoleApi;
  terminalFactory?: TerminalFactory;
  hostOs: string | null;
  /** True when this console was adopted from `console_list` and has history to restore. */
  replay: boolean;
  /** Provider, project and session, for the terminal's accessible name. */
  label: string;
  onClose(consoleId: string): void;
  onStateChange?(state: ConsoleViewState): void;
  onActivity?(): void;
}

export function TerminalPanel(props: TerminalPanelProps) {
  const {
    summary,
    api,
    terminalFactory,
    hostOs,
    replay,
    label,
    onClose,
    onStateChange,
    onActivity,
  } = props;

  return (
    <section className="section" data-testid="terminal-panel">
      <div className="row spread" style={{ marginBottom: 10 }}>
        <h3 style={{ margin: 0 }}>Embedded terminal</h3>
        <span className="status terminal-state">
          <span aria-hidden="true">● </span>
          {STATE_WORDS[summary.state]}
          {summary.exitCode !== null ? ` · exit ${summary.exitCode}` : ""}
        </span>
      </div>
      <div className="terminal-panel">
        <div className="terminal-chrome">
          <span>{PROVIDER_LABELS[summary.provider]} CLI</span>
          <span className="tag">{summary.mode === "resume" ? "resume" : "new session"}</span>
          <span className="cwd" title={summary.cwd}>
            {shortenPath(summary.cwd)}
          </span>
          <span className="spacer" />
          <button type="button" className="icon-btn" onClick={() => onClose(summary.id)}>
            Close terminal
          </button>
        </div>
        {/* Keyed by console id: `ConsoleView` holds `phase`, `exitCode`, `truncated`, `replaying`
            and the resize notice in local state, and without a key React reuses one instance for a
            different console — which put "Process ended — exit code 2" above a live terminal. */}
        <ConsoleView
          key={summary.id}
          id={summary.id}
          api={api}
          createTerminal={terminalFactory}
          hostOs={hostOs}
          replay={replay}
          label={label}
          onStateChange={onStateChange}
          onActivity={onActivity}
        />
      </div>
      <p className="subtle" style={{ marginTop: 10 }}>
        The PTY stays open while Pigeon is running. Selecting another session detaches this view and
        leaves the process alone; closing the terminal ends it and leaves the session and its
        metrics untouched.
      </p>
    </section>
  );
}
