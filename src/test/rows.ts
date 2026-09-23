// Row builders for the component tests.
//
// A builder rather than literals copied into each file, so that a `SessionRow` gaining a field is
// one edit instead of thirty, and so that every test's row is complete — a half-built row makes a
// component look more tolerant of missing data than it is.

import type { Kpis, MetricState, Metrics, ProviderId, SessionRow } from "../bindings";

export const T0 = 1_760_000_000_000;

export function makeKpis(over: Partial<Kpis> = {}): Kpis {
  return { contextPerCall: 112_000, rewriteRatio: 0.09, batchingRatio: 0.73, ...over };
}

export function makeMetrics(over: Partial<Metrics> = {}): Metrics {
  return {
    inputTokens: 1_000,
    outputTokens: 2_000,
    cacheRead: 400_000,
    cacheWrite: 36_000,
    apiCalls: 10,
    toolCalls: 7,
    userTurns: 3,
    durationMs: 600_000,
    reasoningTokens: null,
    providerCostUsd: null,
    kpis: makeKpis(),
    ...over,
  };
}

export function ready(over: Partial<Metrics> = {}): MetricState {
  return { state: "ready", value: makeMetrics(over) };
}

export interface RowSpec {
  provider?: ProviderId;
  sid?: string;
  title?: string;
  project?: string;
  projectName?: string;
  status?: SessionRow["status"];
  lastActiveMs?: number;
  closedAtMs?: number | null;
  metrics?: MetricState;
  resumable?: boolean;
  resumeBlockedReason?: string | null;
}

export function makeRow(spec: RowSpec = {}): SessionRow {
  const provider = spec.provider ?? "claude-code";
  const project = spec.project ?? "/Users/khalid/Documents/Projects/pigeon";
  return {
    key: { providerId: provider, sid: spec.sid ?? "0199c4a1-2b3d-7e4f-8a9b-0c1d2e3f4a5b" },
    cwd: project,
    project,
    projectName: spec.projectName ?? "pigeon",
    projectLeaf: spec.projectName ?? "pigeon",
    title: spec.title ?? "A session",
    name: spec.title ?? "A session",
    gitBranch: "master",
    firstActiveMs: (spec.lastActiveMs ?? T0) - 600_000,
    lastActiveMs: spec.lastActiveMs ?? T0,
    closedAtMs: spec.closedAtMs ?? null,
    resumable: spec.resumable ?? true,
    resumeBlockedReason: spec.resumeBlockedReason ?? null,
    metrics: spec.metrics ?? ready(),
    sourceSummary: null,
    diagnostics: { unknownTypes: {} },
    status: spec.status ?? null,
  };
}
