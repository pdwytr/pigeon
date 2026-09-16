import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { MetricsTotals } from "../bindings";
import { makeKpis, makeMetrics } from "../test/rows";
import { KpiChips } from "./KpiChips";
import { MetricsSection } from "./MetricsSection";

const TOTALS: MetricsTotals = {
  inputTokens: 30_000,
  outputTokens: 50_000,
  cacheRead: 9_000_000,
  cacheWrite: 810_000,
  apiCalls: 90,
  toolCalls: 60,
  userTurns: 30,
  durationMs: 3_600_000,
  reasoningTokens: 0,
  providerCostUsd: 4.18,
};

describe("a KPI that was never measured versus a KPI that measured zero", () => {
  it("renders an absent chip for a null KPI and a real 0.00 for a measured zero", () => {
    render(<KpiChips kpis={makeKpis({ contextPerCall: null, rewriteRatio: 0 })} />);

    const absent = screen.getByTestId("kpi-contextPerCall");
    expect(absent).toHaveAttribute("data-absent", "true");
    expect(absent).toHaveTextContent("—");
    // The failure this test exists for: a null rendered as 0 would tell the owner their context per
    // call is zero, which is a measurement, when in fact nothing was counted.
    expect(absent.textContent).not.toMatch(/\b0\b/);

    const measured = screen.getByTestId("kpi-rewriteRatio");
    expect(measured).toHaveAttribute("data-absent", "false");
    expect(measured).toHaveTextContent("0.00");
  });

  it("names the absence for assistive technology rather than leaving a bare dash", () => {
    render(<KpiChips kpis={makeKpis({ batchingRatio: null })} />);
    expect(screen.getByTestId("kpi-batchingRatio")).toHaveTextContent("not measured");
  });
});

describe("the four metric states", () => {
  it("says counting for a pending fold, and shows neither a blank nor a zero", () => {
    // `queryByText("0")` was the assertion here, on a render containing no numeric text at all —
    // it would have passed against a component that printed nothing, or 0.00, or "zero". What has
    // to be true is that NO counter card is drawn and no digit reaches the screen, because a
    // counter reading 0 and a fold that has not run look identical to the owner and mean opposite
    // things.
    const { container } = render(<MetricsSection scope="session" state={{ state: "pending" }} />);

    expect(screen.getByTestId("metrics-pending")).toHaveTextContent(/counting/i);
    expect(container.querySelectorAll(".metric")).toHaveLength(0);
    expect(screen.queryByText(/\d/)).toBeNull();
    expect(screen.queryByTestId("session-kpi-contextPerCall")).toBeNull();
  });

  it("renders the host's own message for an unavailable metric instead of a zero", () => {
    const message = "The OpenCode database was busy; Pigeon will retry.";
    const { container } = render(
      <MetricsSection
        scope="session"
        state={{
          state: "unavailable",
          error: { provider: "opencode", kind: "busy", detail: { type: "none" }, message },
        }}
      />,
    );

    expect(screen.getByTestId("metrics-unavailable")).toHaveTextContent(message);
    // Same rule as the pending case: no counter cards, and no figure standing in for the failure.
    expect(container.querySelectorAll(".metric")).toHaveLength(0);
    expect(screen.queryByText(/\d/)).toBeNull();
  });

  it("says metrics are not loaded when nothing has been requested yet", () => {
    render(<MetricsSection scope="session" state={null} />);
    expect(screen.getByTestId("metrics-missing")).toHaveTextContent(/not loaded/i);
  });

  it("renders counters and KPIs once the fold is ready", () => {
    // The positive control for the two assertions above: when there IS something to show, there
    // are counter cards and there are digits.
    const { container } = render(
      <MetricsSection
        scope="session"
        state={{ state: "ready", value: makeMetrics({ apiCalls: 84, toolCalls: 61 }) }}
      />,
    );

    expect(screen.getByText("84")).toBeInTheDocument();
    expect(container.querySelectorAll(".metric").length).toBeGreaterThan(0);
    expect(screen.getByTestId("session-kpi-batchingRatio")).toHaveTextContent("0.73");
  });

  it("omits a nullable figure rather than printing it as zero", () => {
    render(
      <MetricsSection
        scope="session"
        state={{
          state: "ready",
          value: makeMetrics({ reasoningTokens: null, providerCostUsd: null }),
        }}
      />,
    );

    expect(screen.queryByText("Reasoning")).toBeNull();
    expect(screen.queryByText("Engine cost")).toBeNull();
  });
});

describe("project KPIs", () => {
  it("renders the host's kpis object and does not recompute one from the totals beside it", () => {
    // The fixture is deliberately inconsistent: cacheRead / apiCalls is 100,000, but the host says
    // the context per call is 112,000. Rust owns the formula — the counting rules are frozen there
    // and a View that recomputed would be a second, undocumented implementation of them. So the
    // number on screen must be the host's, disagreement and all.
    render(
      <MetricsSection
        scope="project"
        totals={TOTALS}
        kpis={makeKpis({ contextPerCall: 112_000, batchingRatio: 0.73 })}
        coverage={{ counted: 3, sessions: 3 }}
        costRows={1}
      />,
    );

    expect(screen.getByTestId("project-kpi-contextPerCall")).toHaveTextContent("112K");
    expect(screen.getByTestId("project-kpi-contextPerCall")).not.toHaveTextContent("100K");
  });

  it("says a project's totals are partial when some of its sessions have not been counted", () => {
    render(
      <MetricsSection
        scope="project"
        totals={TOTALS}
        kpis={makeKpis()}
        coverage={{ counted: 2, sessions: 4 }}
        costRows={1}
      />,
    );

    expect(screen.getByTestId("metrics-coverage")).toHaveTextContent("Counting 2 of 4 sessions");
  });

  it("hides the engine cost entirely when no row stated one", () => {
    render(<MetricsSection scope="project" totals={TOTALS} kpis={makeKpis()} costRows={0} />);
    // Pigeon never invents a price, and $0.00 would read as "this was free".
    expect(screen.queryByText("Engine cost")).toBeNull();
  });
});
