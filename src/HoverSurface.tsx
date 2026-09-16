// What the always-on-top `hover` window renders.
//
// **It is deliberately NOT the dashboard.** That window is declared in `tauri.conf.json` with
// `url: "index.html?surface=hover"`, and until an audit caught it nothing read that parameter — so
// pressing "Show hover" opened a 340×420 undecorated window containing a second complete
// dashboard, with its own five-second poll, its own console subscriptions and its own selection
// state, floating above every other app. Two dashboards, one of them the size of a business card.
//
// This surface asks the host for exactly two things and renders one component. It owns no console,
// project rollup, metrics, or selection state: it is the complete product surface.

import { useCallback, useEffect, useRef, useState } from "react";
import type { FeatherApi } from "./api/types";
import type { AccountStatus, SessionRow, StatusSnapshot } from "./bindings";
import { type HookPrompt, LiveDock } from "./components/LiveDock";
import { formatClock } from "./format";
import { hoverSessions } from "./store/selectors";
import "./styles/feather.css";

/** "Not now" has to survive a restart, or the same offer is made on every launch. */
const HOOKS_DISMISSED_KEY = "pigeon.codex-hooks-dismissed";

/** How long a finished install explains itself before the banner folds away. */
const HOOKS_MESSAGE_MS = 6000;

export interface HoverSurfaceProps {
  api: FeatherApi;
  /** Poll interval. 0 disables the timer, which is what the tests want. */
  pollMs?: number;
}

export default function HoverSurface({ api, pollMs = 5000 }: HoverSurfaceProps) {
  const [status, setStatus] = useState<StatusSnapshot | null>(null);
  const [rows, setRows] = useState<SessionRow[]>([]);
  const [accounts, setAccounts] = useState<
    Partial<Record<AccountStatus["provider"], AccountStatus>>
  >({});
  /** True until the first pair of answers lands — not "true while a poll is in flight". A refresh
   *  that is merely late must not blank a window the owner is reading. */
  const [loading, setLoading] = useState(true);
  const [problem, setProblem] = useState<string | null>(null);
  const [hookPrompt, setHookPrompt] = useState<HookPrompt | null>(null);
  const messageTimer = useRef<number | undefined>(undefined);

  const dismissHooks = useCallback(() => {
    window.localStorage.setItem(HOOKS_DISMISSED_KEY, "true");
    setHookPrompt(null);
  }, []);

  const enableHooks = useCallback(async () => {
    setHookPrompt({ state: "working", message: null, onEnable: () => {}, onDismiss: () => {} });
    let message: string;
    try {
      const report = await api.codexHooksEnable();
      message = report.message;
    } catch {
      message = "Codex did not accept the change.";
    }
    setHookPrompt({ state: "answered", message, onEnable: () => {}, onDismiss: () => {} });
    messageTimer.current = window.setTimeout(() => setHookPrompt(null), HOOKS_MESSAGE_MS);
  }, [api]);

  const load = useCallback(async () => {
    // Settled rather than all: a status read that fails must not also blank the rows, and a row
    // read that fails must not blank the counts. Each half keeps whatever it last had.
    //
    // But `allSettled` DROPS a rejection, and dropping it here is what let an always-on-top
    // window announce "No live sessions" with six agents running — the read had failed and the
    // surface had no way to say so.
    const [snapshot, sessions, accountResult] = await Promise.allSettled([
      api.statusSnapshot(),
      api.sessionsList({ scope: "live" }),
      api.accountStatus(),
    ]);
    if (snapshot.status === "fulfilled") setStatus(snapshot.value);
    if (sessions.status === "fulfilled") setRows(sessions.value.rows);
    if (accountResult.status === "fulfilled") setAccounts(accountResult.value.accounts);
    setProblem(
      failureText(snapshot, sessions, snapshot.status === "fulfilled" ? snapshot.value : null),
    );
    setLoading(false);
  }, [api]);

  useEffect(() => {
    let stopped = false;
    void load();
    // Both signals matter: terminal activity can change state without changing the session list.
    // The hover must report the host's current state, not wait for its five-second safety poll.
    const stopSessions = api.onSessionsChanged((e) => {
      if (!stopped && e.scope === "live") void load();
    });
    const stopStatus = api.onStatusChanged(() => {
      if (!stopped) void load();
    });
    const timer = pollMs > 0 ? setInterval(() => void load(), pollMs) : undefined;
    return () => {
      stopped = true;
      stopSessions();
      stopStatus();
      if (timer !== undefined) clearInterval(timer);
    };
  }, [api, load, pollMs]);

  // The Codex waiting-on-you offer, asked once per window.
  //
  // **A failure here is silent on purpose.** This is an optional capability, not a session read:
  // a host that cannot answer it, or an owner who already said no, must not be shown a problem
  // notice about it. Nothing about the live view depends on the answer.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const { installed } = await api.codexHooksStatus();
        if (cancelled || installed) return;
        if (window.localStorage.getItem(HOOKS_DISMISSED_KEY) === "true") return;
        setHookPrompt({
          state: "asking",
          message: null,
          onEnable: () => void enableHooks(),
          onDismiss: dismissHooks,
        });
      } catch {
        /* no offer, no complaint */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [api, dismissHooks, enableHooks]);

  useEffect(
    () => () => {
      if (messageTimer.current !== undefined) window.clearTimeout(messageTimer.current);
    },
    [],
  );

  // The rows the STATUS says are live, not every row the list returned — the shared selector, so
  // the rule that the rows equal the counts has one implementation and one set of tests.
  const live = hoverSessions(status, rows);

  return (
    <LiveDock
      status={status}
      sessions={live}
      accounts={accounts}
      loading={loading}
      problem={problem}
      hookPrompt={hookPrompt}
    />
  );
}

/**
 * What to say when one of the two reads failed. Names WHICH half failed, because they mean
 * different things — a status the host cannot produce leaves the counts unsafe, a session list it
 * cannot produce leaves the rows unsafe — and says how old what is on screen is, since the surface
 * keeps showing it rather than blanking.
 */
function failureText(
  snapshot: PromiseSettledResult<StatusSnapshot>,
  sessions: PromiseSettledResult<unknown>,
  fresh: StatusSnapshot | null,
): string | null {
  const failed: string[] = [];
  if (snapshot.status === "rejected") failed.push("The live status could not be read.");
  if (sessions.status === "rejected") failed.push("The live session list could not be read.");
  if (failed.length === 0) return null;
  const asOf = fresh ? ` Showing what was read at ${formatClock(fresh.generatedAtMs)}.` : "";
  return `${failed.join(" ")}${asOf}`;
}
