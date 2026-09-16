// The three usefulness KPIs, rendered.
//
// **This component does no arithmetic.** Every number on screen arrives in `Kpis` already computed
// by Rust; the only thing that happens here is `toFixed`. If a division ever appears in this file,
// the boundary has been broken — the formulas are frozen on the host side precisely so that two
// surfaces can never disagree about what "rewrite ratio" means.
//
// The one rule that is easy to get wrong, and is what the tests pin:
//
//   `null` is an ABSENT measurement and renders as a dash in a dimmed chip.
//   `0`    is a REAL measurement and renders as `0.00`.
//
// Rendering a null as zero would tell the owner their cache-rewrite churn is perfect when in fact
// nothing was measured at all — the failure mode that makes a dashboard worse than no dashboard.

import type { Kpis } from "../bindings";
import { ABSENT, formatRatio, formatTokens } from "../format";

interface ChipSpec {
  key: keyof Kpis;
  label: string;
  /** What each KPI means, for the chip's title. Owner diagnostics, never agent targets. */
  hint: string;
  format(value: number): string;
}

const CHIPS: ChipSpec[] = [
  {
    key: "contextPerCall",
    label: "Context / call",
    hint: "Cache reads divided by API calls — how heavy each turn was.",
    format: formatTokens,
  },
  {
    key: "rewriteRatio",
    label: "Rewrite ratio",
    hint: "Cache writes divided by cache reads — stale-resume churn. Low is good.",
    format: formatRatio,
  },
  {
    key: "batchingRatio",
    label: "Batching ratio",
    hint: "Tool calls divided by API calls — parallel-call discipline.",
    format: formatRatio,
  },
];

export interface KpiChipsProps {
  kpis: Kpis;
  /** Where these came from, so a failed assertion names the surface. */
  testIdPrefix?: string;
}

export function KpiChips({ kpis, testIdPrefix = "kpi" }: KpiChipsProps) {
  return (
    <div className="kpi-grid">
      {CHIPS.map((chip) => {
        const value = kpis[chip.key];
        const absent = value === null;
        return (
          <div
            key={chip.key}
            className={absent ? "kpi absent" : "kpi"}
            data-testid={`${testIdPrefix}-${chip.key}`}
            data-absent={absent ? "true" : "false"}
            title={chip.hint}
          >
            <span>{chip.label}</span>
            <b>
              {absent ? ABSENT : chip.format(value)}
              {absent && <span className="sr-only"> not measured</span>}
            </b>
          </div>
        );
      })}
    </div>
  );
}
