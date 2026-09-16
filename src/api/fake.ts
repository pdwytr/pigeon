// An in-memory `FeatherApi`: the whole dashboard, with no Rust behind it.
//
// It exists for two jobs that turn out to be the same job. Tests need a host whose answers they
// chose; `npm run dev` in a plain browser needs a host at all, since only `host_info` exists on the
// Rust side yet. Both get this.
//
// The fixture is built to be AWKWARD on purpose, because the renderings that are easy to get wrong
// are the ones nothing in a happy-path fixture would catch:
//
//   * two sessions from different engines share one `sid` (`SHARED_SID`), so anything keyed on sid
//     alone collapses them and the bug is visible on the first screen rather than in six months;
//   * one session's metrics are `pending`, one's are `unavailable`, the rest are `ready`;
//   * `rewriteRatio: 0` on the codex session is a REAL measurement and must render `0.00`;
//   * every KPI on "Console polish" is `null` (it made no API calls at all) and must render as an
//     absent chip, never as a zero;
//   * OpenCode reports `capacity.supported: false`, which renders "n/a" and draws no bar.

import {
  type AccountStatus,
  type AccountStatusResult,
  type Capacity,
  type CodexHooksReport,
  type CodexHooksStatus,
  type ConsoleScrollback,
  type ConsoleSummary,
  type EngineError,
  type HostInfo,
  type Identity,
  type Kpis,
  type LiveSessionState,
  type MetricState,
  type Metrics,
  type MetricsTotals,
  type ProjectSummary,
  type ProjectSummaryResult,
  type ProviderId,
  type Scope,
  type SessionKey,
  type SessionListResult,
  type SessionRow,
  type SessionsMetricsEvent,
  type Settings,
  type StatusSnapshot,
  type StopResult,
  sessionKeyId,
} from "../bindings";
import { decodeB64, encodeB64 } from "./encoding";
import type {
  ConsoleOpenArgs,
  FeatherApi,
  SessionStartArgs,
  SessionsChangedPayload,
  Unsubscribe,
} from "./types";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;

/** ANSI escape, spelled rather than embedded: a literal ESC byte in a source file is invisible in
 *  every diff and every review. */
const ESC = "\u001b";

/** One uuid, two engines. The whole point of `sessionKeyId`, made visible in the fixture. */
export const SHARED_SID = "0199c4a1-2b3d-7e4f-8a9b-0c1d2e3f4a5b";

const STUDIO_CWD = "/Users/khalid/Documents/Projects/demo-studio";
const FEATHER_CWD = "/Users/khalid/Documents/Projects/feather";
const VAULT_CWD = "/Users/khalid/chat-organization";

function kpis(
  contextPerCall: number | null,
  rewriteRatio: number | null,
  batchingRatio: number | null,
): Kpis {
  return { contextPerCall, rewriteRatio, batchingRatio };
}

function metrics(over: Partial<Metrics> & { kpis: Kpis }): Metrics {
  return {
    inputTokens: 0,
    outputTokens: 0,
    cacheRead: 0,
    cacheWrite: 0,
    apiCalls: 0,
    toolCalls: 0,
    userTurns: 0,
    durationMs: null,
    reasoningTokens: null,
    providerCostUsd: null,
    ...over,
  };
}

function engineError(
  provider: ProviderId | null,
  kind: EngineError["kind"],
  message: string,
): EngineError {
  return { provider, kind, detail: { type: "none" }, message };
}

interface SeedRow {
  row: SessionRow;
  scope: Scope;
}

/** The fixture, rebuilt on every construction so timestamps stay relative to "now". */
function seed(now: number): SeedRow[] {
  const base = {
    gitBranch: "master" as string | null,
    resumeBlockedReason: null as string | null,
    diagnostics: { unknownTypes: {} },
  };

  return [
    {
      scope: "live",
      row: {
        ...base,
        key: { providerId: "claude-code", sid: SHARED_SID },
        cwd: STUDIO_CWD,
        project: STUDIO_CWD,
        projectName: "demo-studio",
        projectLeaf: "demo-studio",
        title: "Pigeon UI",
        name: "Pigeon UI",
        firstActiveMs: now - 92 * MINUTE,
        lastActiveMs: now - 15_000,
        closedAtMs: null,
        resumable: true,
        sourceSummary: "claude-code transcript, 318 records",
        status: "running",
        metrics: {
          state: "ready",
          value: metrics({
            inputTokens: 41_200,
            outputTokens: 88_400,
            cacheRead: 9_408_000,
            cacheWrite: 846_720,
            apiCalls: 84,
            toolCalls: 61,
            userTurns: 27,
            durationMs: 92 * MINUTE,
            reasoningTokens: 12_800,
            providerCostUsd: 4.18,
            kpis: kpis(112_000, 0.09, 0.73),
          }),
        },
      },
    },
    {
      scope: "live",
      row: {
        ...base,
        // The same sid as the row above, from a different engine. Both must render.
        key: { providerId: "codex", sid: SHARED_SID },
        cwd: STUDIO_CWD,
        project: STUDIO_CWD,
        projectName: "demo-studio",
        projectLeaf: "demo-studio",
        title: "Fix test suite",
        name: "Fix test suite",
        gitBranch: "lane/tests",
        firstActiveMs: now - 26 * MINUTE,
        lastActiveMs: now - 4 * MINUTE,
        closedAtMs: null,
        resumable: true,
        sourceSummary: "codex rollout, 141 records",
        status: "needs_you",
        metrics: {
          state: "ready",
          value: metrics({
            inputTokens: 18_900,
            outputTokens: 21_500,
            cacheRead: 3_100_000,
            // Zero cache writes is a real measurement, and `rewriteRatio` below is a real 0.00.
            cacheWrite: 0,
            apiCalls: 31,
            toolCalls: 22,
            userTurns: 11,
            durationMs: 26 * MINUTE,
            reasoningTokens: 4_300,
            providerCostUsd: null,
            kpis: kpis(100_000, 0, 0.71),
          }),
        },
      },
    },
    {
      scope: "live",
      row: {
        ...base,
        key: { providerId: "opencode", sid: "0199c4b8-77aa-7c31-9f20-5d6e7f809a1b" },
        cwd: FEATHER_CWD,
        project: FEATHER_CWD,
        projectName: "feather",
        projectLeaf: "feather",
        title: "API adapter",
        name: null,
        gitBranch: null,
        firstActiveMs: now - 40 * MINUTE,
        lastActiveMs: now - 18 * MINUTE,
        closedAtMs: null,
        resumable: false,
        resumeBlockedReason: "OpenCode does not publish a resume command for this session.",
        sourceSummary: null,
        // No status at all: a live process the engine says nothing about. Renders an absent status.
        status: null,
        metrics: { state: "pending" },
      },
    },
    {
      scope: "live",
      row: {
        ...base,
        key: { providerId: "claude-code", sid: "0199c4c2-1188-7d55-b0e4-6a7b8c9d0e1f" },
        cwd: STUDIO_CWD,
        project: STUDIO_CWD,
        projectName: "demo-studio",
        projectLeaf: "demo-studio",
        title: "Untitled session",
        name: null,
        firstActiveMs: now - 70 * MINUTE,
        lastActiveMs: now - 61 * MINUTE,
        closedAtMs: null,
        resumable: true,
        sourceSummary: null,
        status: "running",
        metrics: {
          state: "unavailable",
          error: engineError(
            "claude-code",
            "unknown_shape",
            "Claude Code wrote a record shape Pigeon does not recognize; metrics were not counted.",
          ),
        },
      },
    },
    {
      scope: "recent",
      row: {
        ...base,
        key: { providerId: "claude-code", sid: "0199c3f0-4411-7a02-8c31-2d3e4f506172" },
        cwd: FEATHER_CWD,
        project: FEATHER_CWD,
        projectName: "feather",
        projectLeaf: "feather",
        title: "Console polish",
        name: "Console polish",
        firstActiveMs: now - 5 * HOUR,
        lastActiveMs: now - 3 * HOUR,
        closedAtMs: now - 3 * HOUR,
        resumable: true,
        sourceSummary: "claude-code transcript, 4 records",
        status: "finished",
        metrics: {
          state: "ready",
          // Opened, typed nothing, closed. Every KPI is undefined because every denominator is
          // zero, and every one of them must render as an absent chip, not as `0`.
          value: metrics({ userTurns: 1, durationMs: 2 * HOUR, kpis: kpis(null, null, null) }),
        },
      },
    },
    {
      scope: "recent",
      row: {
        ...base,
        key: { providerId: "codex", sid: "0199c3d4-9922-7b18-af53-1c2d3e4f5061" },
        cwd: FEATHER_CWD,
        project: FEATHER_CWD,
        projectName: "feather",
        projectLeaf: "feather",
        title: "Docs cleanup",
        name: "Docs cleanup",
        firstActiveMs: now - 4 * HOUR,
        lastActiveMs: now - 2 * HOUR,
        closedAtMs: now - 2 * HOUR,
        resumable: true,
        sourceSummary: "codex rollout, 62 records",
        status: "finished",
        metrics: {
          state: "ready",
          value: metrics({
            inputTokens: 9_100,
            outputTokens: 14_200,
            cacheRead: 972_000,
            cacheWrite: 106_920,
            apiCalls: 18,
            toolCalls: 12,
            userTurns: 6,
            durationMs: 2 * HOUR,
            reasoningTokens: null,
            providerCostUsd: 0.94,
            kpis: kpis(54_000, 0.11, 0.67),
          }),
        },
      },
    },
    {
      scope: "recent",
      row: {
        ...base,
        key: { providerId: "claude-code", sid: "0199c3aa-5533-7e77-9021-0a1b2c3d4e5f" },
        cwd: STUDIO_CWD,
        project: STUDIO_CWD,
        projectName: "demo-studio",
        projectLeaf: "demo-studio",
        title: "Review parser",
        name: "Review parser",
        firstActiveMs: now - 2 * HOUR,
        lastActiveMs: now - 31 * MINUTE,
        closedAtMs: now - 31 * MINUTE,
        resumable: true,
        sourceSummary: "claude-code transcript, 96 records",
        status: "finished",
        metrics: {
          state: "ready",
          value: metrics({
            inputTokens: 22_400,
            outputTokens: 31_800,
            cacheRead: 2_130_000,
            cacheWrite: 234_300,
            apiCalls: 27,
            toolCalls: 31,
            userTurns: 9,
            durationMs: 89 * MINUTE,
            reasoningTokens: 6_100,
            providerCostUsd: 1.37,
            kpis: kpis(78_888, 0.11, 1.15),
          }),
        },
      },
    },
    {
      scope: "recent",
      row: {
        ...base,
        key: { providerId: "opencode", sid: "0199c2b1-6644-7f09-8123-9e8d7c6b5a40" },
        cwd: VAULT_CWD,
        project: VAULT_CWD,
        projectName: "chat-organization",
        projectLeaf: "chat-organization",
        title: "Vault sweep",
        name: null,
        gitBranch: null,
        firstActiveMs: now - 27 * HOUR,
        lastActiveMs: now - 26 * HOUR,
        closedAtMs: now - 26 * HOUR,
        resumable: false,
        resumeBlockedReason: "The OpenCode session store no longer holds this session.",
        sourceSummary: null,
        status: "finished",
        metrics: {
          state: "unavailable",
          error: engineError(
            "opencode",
            "busy",
            "The OpenCode database was busy; Pigeon will retry.",
          ),
        },
      },
    },
  ];
}

function emptyTotals(): MetricsTotals {
  return {
    inputTokens: 0,
    outputTokens: 0,
    cacheRead: 0,
    cacheWrite: 0,
    apiCalls: 0,
    toolCalls: 0,
    userTurns: 0,
    durationMs: 0,
    reasoningTokens: 0,
    providerCostUsd: 0,
  };
}

/**
 * Aggregate the fixture's rows the way RUST would.
 *
 * This lives in the fake, not in the app: `ProjectSummary.totals` and `ProjectSummary.kpis` arrive
 * from the host, and no React module is allowed to compute either. A fake host is a host, so it is
 * the right place, and keeping it here is what lets a test hand the View a `kpis` that disagrees
 * with its own `totals` and prove the View rendered the `kpis`.
 */
export function summarize(rows: SessionRow[]): ProjectSummary[] {
  const byProject = new Map<string, SessionRow[]>();
  for (const row of rows) {
    const list = byProject.get(row.project);
    if (list) list.push(row);
    else byProject.set(row.project, [row]);
  }
  const out: ProjectSummary[] = [];
  for (const [project, list] of byProject) {
    const totals = emptyTotals();
    const providers: Partial<Record<ProviderId, number>> = {};
    const statusCounts = { running: 0, needsYou: 0, finished: 0, unknown: 0 };
    let counted = 0;
    let costRows = 0;
    let lastActiveMs = 0;
    for (const row of list) {
      providers[row.key.providerId] = (providers[row.key.providerId] ?? 0) + 1;
      lastActiveMs = Math.max(lastActiveMs, row.lastActiveMs);
      if (row.status === "running") statusCounts.running += 1;
      else if (row.status === "needs_you") statusCounts.needsYou += 1;
      else if (row.status === "finished") statusCounts.finished += 1;
      else statusCounts.unknown += 1;
      if (row.metrics.state !== "ready") continue;
      counted += 1;
      const m = row.metrics.value;
      totals.inputTokens += m.inputTokens;
      totals.outputTokens += m.outputTokens;
      totals.cacheRead += m.cacheRead;
      totals.cacheWrite += m.cacheWrite;
      totals.apiCalls += m.apiCalls;
      totals.toolCalls += m.toolCalls;
      totals.userTurns += m.userTurns;
      totals.durationMs += m.durationMs ?? 0;
      totals.reasoningTokens += m.reasoningTokens ?? 0;
      if (m.providerCostUsd !== null) {
        totals.providerCostUsd += m.providerCostUsd;
        costRows += 1;
      }
    }
    out.push({
      project,
      projectName: list[0].projectName,
      cwd: list[0].cwd ?? project,
      projectLeaf: list[0].projectLeaf,
      sessions: list.length,
      counted,
      providers,
      statusCounts,
      totals,
      // Recomputed from the totals, exactly as the contract requires of the host, never averaged
      // from the rows' own KPIs.
      kpis: kpis(
        totals.apiCalls ? Math.round(totals.cacheRead / totals.apiCalls) : null,
        totals.cacheRead ? totals.cacheWrite / totals.cacheRead : null,
        totals.apiCalls ? totals.toolCalls / totals.apiCalls : null,
      ),
      lastActiveMs,
      costRows,
    });
  }
  return out.sort((a, b) => b.lastActiveMs - a.lastActiveMs);
}

function identity(provider: ProviderId, label: string, plan: string): Identity {
  return {
    provider,
    signedIn: true,
    label,
    organization: "IntAnalytic",
    plan,
    tier: null,
    mode: "subscription",
    accountShort: label,
    providers: null,
    readAtMs: Date.now(),
    problem: null,
  };
}

function capacity(
  provider: ProviderId,
  supported: boolean,
  windows: Capacity["windows"],
): Capacity {
  return {
    provider,
    supported,
    windows,
    plan: supported ? "Max" : null,
    stale: false,
    sourceAgeS: 42,
    reachedLimit: null,
    readAtMs: Date.now(),
    problem: null,
  };
}

export interface FakeApiOptions {
  /**
   * Let the fake behave like a live host: resolve the pending metric a beat after startup, and echo
   * keystrokes. Off by default, because a test that finishes before a timer fires would otherwise
   * update React state after its own teardown.
   */
  animate?: boolean;
  hostOs?: string;
}

/** The fake, a class so a test can reach the scripting side (`emit`, `emitExit`) that no production
 *  caller has. */
export class FakeFeatherApi implements FeatherApi {
  readonly calls: { command: string; args?: unknown }[] = [];
  private readonly now = Date.now();
  private readonly seedRows = seed(this.now);
  private readonly os: string;
  private readonly animate: boolean;
  private consoles = new Map<string, ConsoleSummary>();
  private scrollback = new Map<string, string>();
  private readied = new Set<string>();
  private held = new Map<string, string[]>();
  private settings: Settings = {
    hover: { visible: false, corner: "tr", x: null, y: null },
    list: { view: "live" },
    pollIntervalSeconds: 5,
    recentWindowDays: 7,
    split: 0.34,
  };
  private hoverVisible = false;
  private codexHooksInstalled = false;
  private seq = 0;
  private lateMetricsScheduled = false;
  private subs = {
    sessionsChanged: new Set<(e: SessionsChangedPayload) => void>(),
    metrics: new Set<(e: SessionsMetricsEvent) => void>(),
    status: new Set<(e: StatusSnapshot) => void>(),
    data: new Set<(e: { id: string; dataB64: string }) => void>(),
    exit: new Set<(e: { id: string; exitCode: number | null }) => void>(),
    select: new Set<(k: SessionKey) => void>(),
  };

  constructor(options: FakeApiOptions = {}) {
    this.animate = options.animate ?? false;
    this.os = options.hostOs ?? "macos";
    // One console already open and DETACHED. The reattach path (find it in `console_list`, replay
    // its scrollback, then go live) is the one worth having on screen from the first second.
    const replay =
      `codex resume ${ESC}[2m--session 0199c4a1${ESC}[0m\r\n\r\n` +
      `  ${ESC}[32m>${ESC}[0m running the failing case again\r\n` +
      `  ${ESC}[2m3 files changed, 2 tests still red${ESC}[0m\r\n\r\n` +
      `  ${ESC}[31m>${ESC}[0m waiting for you: apply the fix to adapters/codex.py? (y/n) `;
    const detached: ConsoleSummary = {
      id: "con-seed-1",
      sessionKey: { providerId: "codex", sid: SHARED_SID },
      provider: "codex",
      cwd: STUDIO_CWD,
      mode: "resume",
      state: "running",
      cols: 80,
      rows: 24,
      scrollbackBytes: new TextEncoder().encode(replay).length,
      exitCode: null,
      startedAtMs: this.now - 5 * MINUTE,
    };
    this.scrollback.set(detached.id, replay);
    this.consoles.set(detached.id, detached);
  }

  private record(command: string, args?: unknown) {
    this.calls.push({ command, args });
  }

  private rowsFor(scope: Scope): SessionRow[] {
    return this.seedRows.filter((r) => r.scope === scope).map((r) => ({ ...r.row }));
  }

  /** Every command this fake was asked to perform, in order, for order assertions. */
  commandNames(): string[] {
    return this.calls.map((c) => c.command);
  }

  async hostInfo(): Promise<HostInfo> {
    this.record("host_info");
    return { os: this.os, arch: "aarch64", version: "0.1.0" };
  }

  async settingsGet(): Promise<Settings> {
    this.record("settings_get");
    return { ...this.settings };
  }

  async settingsSet(patch: Partial<Settings>): Promise<Settings> {
    this.record("settings_set", patch);
    this.settings = { ...this.settings, ...patch };
    return { ...this.settings };
  }

  async sessionsList(args: { scope: Scope; force?: boolean }): Promise<SessionListResult> {
    this.record("sessions_list", args);
    if (this.animate) this.scheduleLateMetrics();
    return {
      scope: args.scope,
      sinceMs: args.scope === "recent" ? this.now - 7 * 24 * HOUR : null,
      rows: this.rowsFor(args.scope),
      // One engine's failure travels beside the other two's rows rather than replacing them.
      problems:
        args.scope === "recent"
          ? [engineError("opencode", "busy", "The OpenCode database was busy; Pigeon will retry.")]
          : [],
      generatedAtMs: Date.now(),
    };
  }

  async projectsSummary(args: { scope: Scope }): Promise<ProjectSummaryResult> {
    this.record("projects_summary", args);
    return {
      scope: args.scope,
      sinceMs: args.scope === "recent" ? this.now - 7 * 24 * HOUR : null,
      projects: summarize(this.rowsFor(args.scope)),
      generatedAtMs: Date.now(),
    };
  }

  async sessionMetrics(args: { key: SessionKey }): Promise<MetricState> {
    this.record("session_metrics", args);
    const id = sessionKeyId(args.key);
    const found = this.seedRows.find((r) => sessionKeyId(r.row.key) === id);
    return found ? found.row.metrics : { state: "pending" };
  }

  async statusSnapshot(): Promise<StatusSnapshot> {
    this.record("status_snapshot");
    return this.buildStatus();
  }

  private buildStatus(): StatusSnapshot {
    const live: LiveSessionState[] = this.seedRows
      .filter((r) => r.scope === "live" && r.row.status !== null && r.row.status !== "finished")
      .map((r) => ({
        key: r.row.key,
        process: "present",
        state:
          r.row.status === "needs_you"
            ? "needs_you"
            : r.row.status === "running"
              ? "running"
              : "unknown",
        projectLeaf: r.row.projectLeaf,
        title: r.row.title,
        name: r.row.name,
        sinceMs: r.row.lastActiveMs,
        rawWord: r.row.status === "needs_you" ? "awaiting_input" : "working",
        evidence: [`pid ${4200 + r.row.title.length}`, `cwd ${r.row.cwd}`],
        pid: 4200 + r.row.title.length,
        consoleId: null,
        observedAtMs: Date.now(),
      }));
    return {
      generatedAtMs: Date.now(),
      counts: {
        running: live.filter((l) => l.state === "running").length,
        needsYou: live.filter((l) => l.state === "needs_you").length,
        unknown: live.filter((l) => l.state === "unknown").length,
      },
      live,
    };
  }

  async accountStatus(): Promise<AccountStatusResult> {
    this.record("account_status");
    const accounts: Partial<Record<ProviderId, AccountStatus>> = {
      "claude-code": {
        provider: "claude-code",
        identity: identity("claude-code", "mshaik@intanalytic.com", "Max"),
        capacity: capacity("claude-code", true, [
          {
            name: "five_hour",
            windowMinutes: 300,
            usedPct: 42,
            resetsAtMs: this.now + 96 * MINUTE,
          },
          {
            name: "weekly",
            windowMinutes: 10_080,
            usedPct: 68,
            resetsAtMs: this.now + 3 * 24 * HOUR,
          },
        ]),
      },
      codex: {
        provider: "codex",
        identity: identity("codex", "mshaik@intanalytic.com", "Pro"),
        capacity: capacity("codex", true, [
          {
            name: "five_hour",
            windowMinutes: 300,
            usedPct: 12,
            resetsAtMs: this.now + 210 * MINUTE,
          },
        ]),
      },
      // OpenCode publishes no allowance at all. "n/a", never a zero-width bar.
      opencode: {
        provider: "opencode",
        identity: identity("opencode", "local", "self-hosted"),
        capacity: capacity("opencode", false, []),
      },
    };
    return { accounts, generatedAtMs: Date.now() };
  }

  async projectPick(): Promise<{ cwd: string } | null> {
    this.record("project_pick");
    return { cwd: "/Users/khalid/Documents/Projects/new-project" };
  }

  async folderOpen(args: { cwd: string }): Promise<void> {
    this.record("folder_open", args);
  }

  async sessionStart(args: SessionStartArgs): Promise<{ id: string }> {
    this.record("session_start", args);
    return this.spawn(args.provider, args.cwd, null, "new", args.cols, args.rows);
  }

  async sessionStop(args: { key: SessionKey }): Promise<StopResult> {
    this.record("session_stop", args);
    const found = this.seedRows.find((r) => sessionKeyId(r.row.key) === sessionKeyId(args.key));
    if (found) found.row.status = "finished";
    for (const cb of [...this.subs.status]) cb(this.buildStatus());
    return {
      key: args.key,
      stopped: [{ pid: 4242, evidence: "matched cwd and argv" }],
      alreadyStopped: false,
      ambiguous: [],
    };
  }

  async consoleOpen(args: ConsoleOpenArgs): Promise<{ id: string }> {
    this.record("console_open", args);
    return this.spawn(
      args.sessionKey.providerId,
      args.cwd,
      args.sessionKey,
      "resume",
      args.cols,
      args.rows,
    );
  }

  private spawn(
    provider: ProviderId,
    cwd: string,
    sessionKey: SessionKey | null,
    mode: "resume" | "new",
    cols: number,
    rows: number,
  ): { id: string } {
    this.seq += 1;
    const id = `con-${this.seq}`;
    this.consoles.set(id, {
      id,
      sessionKey,
      provider,
      cwd,
      mode,
      state: "running",
      cols,
      rows,
      scrollbackBytes: 0,
      exitCode: null,
      startedAtMs: Date.now(),
    });
    const banner =
      `${ESC}[1m${provider}${ESC}[0m ${mode === "resume" ? "resume" : "new session"}\r\n` +
      `working directory: ${cwd}\r\n\r\n${ESC}[32m>${ESC}[0m `;
    this.emit(id, banner);
    return { id };
  }

  async consoleReady(args: { id: string }): Promise<void> {
    this.record("console_ready", args);
    if (this.readied.has(args.id)) return;
    this.readied.add(args.id);
    // The real host gates its output pump on this call; so does the fake, or a test could never
    // tell a correctly ordered attach from an incorrectly ordered one.
    for (const text of this.held.get(args.id)?.splice(0) ?? []) this.deliver(args.id, text);
  }

  async consoleInput(args: { id: string; dataB64: string }): Promise<void> {
    this.record("console_input", args);
    if (!this.animate) return;
    const text = new TextDecoder().decode(decodeB64(args.dataB64));
    // A terminal echoes; Enter arrives as a bare CR and has to be paired with LF or the next line
    // overwrites the current one. That is the only terminal behaviour this fake models.
    this.emit(args.id, text.replace(/\r/g, `\r\n${ESC}[32m>${ESC}[0m `));
  }

  async consoleResize(args: { id: string; cols: number; rows: number }): Promise<void> {
    this.record("console_resize", args);
    const row = this.consoles.get(args.id);
    if (row) this.consoles.set(args.id, { ...row, cols: args.cols, rows: args.rows });
  }

  async consoleClose(args: { id: string }): Promise<void> {
    this.record("console_close", args);
    const row = this.consoles.get(args.id);
    if (!row) return;
    this.consoles.set(args.id, { ...row, state: "closed", exitCode: null });
    for (const cb of [...this.subs.exit]) cb({ id: args.id, exitCode: null });
  }

  async consoleList(): Promise<{ consoles: ConsoleSummary[] }> {
    this.record("console_list");
    return { consoles: [...this.consoles.values()].map((c) => ({ ...c })) };
  }

  async consoleScrollback(args: { id: string; maxBytes?: number }): Promise<ConsoleScrollback> {
    this.record("console_scrollback", args);
    const text = this.scrollback.get(args.id) ?? "";
    return { id: args.id, dataB64: encodeB64(text), truncated: false };
  }

  async hoverToggle(): Promise<{ visible: boolean }> {
    this.record("hover_toggle");
    this.hoverVisible = !this.hoverVisible;
    this.settings = {
      ...this.settings,
      hover: { ...this.settings.hover, visible: this.hoverVisible },
    };
    return { visible: this.hoverVisible };
  }

  async hoverSelect(key: SessionKey): Promise<void> {
    this.record("hover_select", key);
    for (const cb of [...this.subs.select]) cb(key);
  }

  async codexHooksStatus(): Promise<CodexHooksStatus> {
    this.record("codex_hooks_status");
    return { installed: this.codexHooksInstalled };
  }

  async codexHooksEnable(): Promise<CodexHooksReport> {
    this.record("codex_hooks_enable");
    this.codexHooksInstalled = true;
    return {
      installed: true,
      trusted: true,
      message: "Pigeon will now show when Codex is waiting on you.",
    };
  }

  async codexHooksDisable(): Promise<void> {
    this.record("codex_hooks_disable");
    this.codexHooksInstalled = false;
  }

  onSessionsChanged(cb: (e: SessionsChangedPayload) => void): Unsubscribe {
    this.subs.sessionsChanged.add(cb);
    return () => void this.subs.sessionsChanged.delete(cb);
  }
  onSessionsMetrics(cb: (e: SessionsMetricsEvent) => void): Unsubscribe {
    this.subs.metrics.add(cb);
    return () => void this.subs.metrics.delete(cb);
  }
  onStatusChanged(cb: (e: StatusSnapshot) => void): Unsubscribe {
    this.subs.status.add(cb);
    return () => void this.subs.status.delete(cb);
  }
  onConsoleData(cb: (e: { id: string; dataB64: string }) => void): Unsubscribe {
    this.subs.data.add(cb);
    return () => void this.subs.data.delete(cb);
  }
  onConsoleExit(cb: (e: { id: string; exitCode: number | null }) => void): Unsubscribe {
    this.subs.exit.add(cb);
    return () => void this.subs.exit.delete(cb);
  }
  onSelectSession(cb: (k: SessionKey) => void): Unsubscribe {
    this.subs.select.add(cb);
    return () => void this.subs.select.delete(cb);
  }

  // ------------------------------------------------------------------- the scripting side

  /** Say something as the host would, held until `console_ready` exactly as the host holds it. */
  emit(id: string, text: string): void {
    // The host's scrollback grows whether or not a view is attached — that is what makes leaving a
    // console and coming back to it replay anything at all. Recorded before the gate, because the
    // held output is output.
    this.remember(id, text);
    if (!this.readied.has(id)) {
      const queue = this.held.get(id);
      if (queue) queue.push(text);
      else this.held.set(id, [text]);
      return;
    }
    this.deliver(id, text);
  }

  /** Append to a console's bounded history and keep its `scrollbackBytes` honest, which is what
   *  the view checks before asking for a replay. */
  private remember(id: string, text: string): void {
    const next = (this.scrollback.get(id) ?? "") + text;
    this.scrollback.set(id, next);
    const row = this.consoles.get(id);
    if (row) {
      this.consoles.set(id, {
        ...row,
        scrollbackBytes: new TextEncoder().encode(next).length,
      });
    }
  }

  private deliver(id: string, text: string): void {
    for (const cb of [...this.subs.data]) cb({ id, dataB64: encodeB64(text) });
  }

  /** Land a lazy metric fold, exactly as the host's `sessions://metrics` event does — and update
   *  the fixture's own row, so a later `sessions_list` agrees with the event. */
  emitMetrics(rows: { key: SessionKey; metrics: MetricState }[]): void {
    for (const row of rows) {
      const seeded = this.seedRows.find((r) => sessionKeyId(r.row.key) === sessionKeyId(row.key));
      if (seeded) seeded.row.metrics = row.metrics;
    }
    const event: SessionsMetricsEvent = { rows, generatedAtMs: Date.now() };
    for (const cb of [...this.subs.metrics]) cb(event);
  }

  /** End a console as the host would. */
  emitExit(id: string, exitCode: number | null): void {
    const row = this.consoles.get(id);
    if (row) this.consoles.set(id, { ...row, state: "exited", exitCode });
    for (const cb of [...this.subs.exit]) cb({ id, exitCode });
  }

  /** The pending metric lands a beat later, as a real lazy fold would. */
  private scheduleLateMetrics(): void {
    if (this.lateMetricsScheduled) return;
    this.lateMetricsScheduled = true;
    const pendingRow = this.seedRows.find((r) => r.row.metrics.state === "pending");
    if (!pendingRow) return;
    setTimeout(() => {
      pendingRow.row.metrics = {
        state: "ready",
        value: metrics({
          inputTokens: 5_400,
          outputTokens: 7_100,
          cacheRead: 504_000,
          cacheWrite: 20_160,
          apiCalls: 12,
          toolCalls: 19,
          userTurns: 4,
          durationMs: 22 * MINUTE,
          reasoningTokens: null,
          providerCostUsd: null,
          kpis: kpis(42_000, 0.04, 1.58),
        }),
      };
      const event: SessionsMetricsEvent = {
        rows: [{ key: pendingRow.row.key, metrics: pendingRow.row.metrics }],
        generatedAtMs: Date.now(),
      };
      for (const cb of [...this.subs.metrics]) cb(event);
    }, 1600);
  }
}

export function createFakeApi(options?: FakeApiOptions): FakeFeatherApi {
  return new FakeFeatherApi(options);
}

/**
 * The fake with some of its commands replaced — how a test makes ONE command fail without
 * inventing a second host and without teaching the fake a failure mode per test.
 *
 * The overrides sit on an object whose prototype is the fake, so every command that was not
 * replaced still runs the fixture's own code and still records itself in `calls`.
 */
export function fakeApiWith(over: Partial<FeatherApi>, options?: FakeApiOptions): FakeFeatherApi {
  return Object.assign(Object.create(createFakeApi(options)), over) as FakeFeatherApi;
}
