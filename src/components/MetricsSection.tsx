// Counters and KPIs for one project or one session.
//
// Four states, four renderings, and the contract's table is the whole specification:
//
//   missing      "Metrics not loaded"        never a zero as a default
//   pending      "Counting…"                 never a blank card that looks final
//   ready        counters and KPIs           never a formula recomputed here
//   unavailable  the host's own message      never a zero standing in for the failure
//
// The distinction is not cosmetic. A pending fold and a zero-token session look identical if both
// render `0`, and the owner has no way to tell "Pigeon has not counted this yet" from "this
// session did nothing" — which are opposite conclusions about whether to go and look at it.

import type { Kpis, MetricState, Metrics, MetricsTotals } from "../bindings";
import { formatCount, formatDuration, formatTokens, formatUsd } from "../format";
import { KpiChips } from "./KpiChips";

export interface MetricsSectionProps {
  scope: "project" | "session";
  /** The session's metric state. `null` means nothing has been requested for it yet. */
  state?: MetricState | null;
  /** A project's summed counters, which arrive already summed from the host. */
  totals?: MetricsTotals;
  /** A project's KPIs, recomputed by the host from `totals`. Never derived here from `totals`. */
  kpis?: Kpis;
  /** How many of a project's sessions contributed metrics, and how many there are in all. */
  coverage?: { counted: number; sessions: number };
  /** How many rows stated a cost. Zero hides the figure entirely — Pigeon never invents a price. */
  costRows?: number;
  onRetry?: () => void;
}

interface Cell {
  label: string;
  value: string;
}

function sessionCells(m: Metrics): Cell[] {
  const cells: Cell[] = [
    { label: "Tokens in", value: formatTokens(m.inputTokens) },
    { label: "Tokens out", value: formatTokens(m.outputTokens) },
    { label: "Cache read", value: formatTokens(m.cacheRead) },
    { label: "Cache write", value: formatTokens(m.cacheWrite) },
    { label: "API calls", value: formatCount(m.apiCalls) },
    { label: "Tool calls", value: formatCount(m.toolCalls) },
    { label: "Your turns", value: formatCount(m.userTurns) },
  ];
  // Nullable figures are OMITTED rather than shown as zero. A session with no reasoning tokens and
  // one whose engine does not report them are different facts, and neither of them is "0".
  if (m.durationMs !== null) cells.push({ label: "Active", value: formatDuration(m.durationMs) });
  if (m.reasoningTokens !== null) {
    cells.push({ label: "Reasoning", value: formatTokens(m.reasoningTokens) });
  }
  if (m.providerCostUsd !== null) {
    cells.push({ label: "Engine cost", value: formatUsd(m.providerCostUsd) });
  }
  return cells;
}

function projectCells(t: MetricsTotals, costRows: number): Cell[] {
  const cells: Cell[] = [
    { label: "Tokens in", value: formatTokens(t.inputTokens) },
    { label: "Tokens out", value: formatTokens(t.outputTokens) },
    { label: "Cache read", value: formatTokens(t.cacheRead) },
    { label: "Cache write", value: formatTokens(t.cacheWrite) },
    { label: "API calls", value: formatCount(t.apiCalls) },
    { label: "Tool calls", value: formatCount(t.toolCalls) },
    { label: "Your turns", value: formatCount(t.userTurns) },
    { label: "Active", value: formatDuration(t.durationMs) },
  ];
  // Zero contributing rows hides the cost, rather than showing a $0.00 that would read as free.
  if (costRows > 0) cells.push({ label: "Engine cost", value: formatUsd(t.providerCostUsd) });
  return cells;
}

function Grid({ cells }: { cells: Cell[] }) {
  return (
    <div className="metric-grid">
      {cells.map((cell) => (
        <div className="metric" key={cell.label}>
          <b>{cell.value}</b>
          <span>{cell.label}</span>
        </div>
      ))}
    </div>
  );
}

export function MetricsSection(props: MetricsSectionProps) {
  const { scope, state, totals, kpis, coverage, costRows = 0, onRetry } = props;

  if (scope === "project") {
    if (!totals || !kpis) {
      return (
        <section className="section" data-testid="metrics-project">
          <h3>Project metrics</h3>
          <p className="subtle">Metrics not loaded.</p>
        </section>
      );
    }
    const partial = coverage && coverage.counted < coverage.sessions;
    return (
      <section className="section" data-testid="metrics-project">
        <h3>Project metrics</h3>
        <Grid cells={projectCells(totals, costRows)} />
        <h3 style={{ marginTop: 16 }}>Project usefulness</h3>
        {/* Straight from `ProjectSummary.kpis`. Not averaged from the rows, not derived from the
            totals above — the host recomputes them and the View shows what it was handed. */}
        <KpiChips kpis={kpis} testIdPrefix="project-kpi" />
        {partial && coverage && (
          <p className="pending" data-testid="metrics-coverage">
            Counting {coverage.sessions - coverage.counted} of {coverage.sessions} sessions — these
            totals are partial.
          </p>
        )}
      </section>
    );
  }

  if (!state) {
    return (
      <section className="section" data-testid="metrics-session">
        <h3>Session metrics</h3>
        <p className="subtle" data-testid="metrics-missing">
          Metrics not loaded.
        </p>
      </section>
    );
  }

  if (state.state === "pending") {
    return (
      <section className="section" data-testid="metrics-session">
        <h3>Session metrics</h3>
        <p className="pending" data-testid="metrics-pending">
          Counting…
        </p>
      </section>
    );
  }

  if (state.state === "unavailable") {
    return (
      <section className="section" data-testid="metrics-session">
        <h3>Session metrics</h3>
        {/* The host's own sentence, rendered and never parsed. */}
        <div className="unavailable" data-testid="metrics-unavailable" role="status">
          Metrics unavailable — {state.error.message}
        </div>
        {onRetry && (
          <div className="row" style={{ marginTop: 8 }}>
            <button type="button" className="icon-btn" onClick={onRetry}>
              Try again
            </button>
          </div>
        )}
      </section>
    );
  }

  return (
    <section className="section" data-testid="metrics-session">
      <h3>Session metrics</h3>
      <Grid cells={sessionCells(state.value)} />
      <h3 style={{ marginTop: 16 }}>Session usefulness</h3>
      <KpiChips kpis={state.value.kpis} testIdPrefix="session-kpi" />
    </section>
  );
}
