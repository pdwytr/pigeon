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

/** "Not now" has to survive a restart, or the same offer is made on every launch. One key per
 *  engine, so dismissing the Codex offer does not silently dismiss OpenCode's. */
const CODEX_DISMISSED_KEY = "pigeon.codex-hooks-dismissed";
const OPENCODE_DISMISSED_KEY = "pigeon.opencode-hooks-dismissed";

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
  const [hookPrompts, setHookPrompts] = useState<HookPrompt[]>([]);
  const messageTimer = useRef<number | undefined>(undefined);

  const setPrompt = useCallback((engine: HookPrompt["engine"], next: HookPrompt | null) => {
    setHookPrompts((current) => {
      const others = current.filter((prompt) => prompt.engine !== engine);
      return next ? [...others, next] : others;
    });
  }, []);

  const dismissHooks = useCallback(
    (engine: HookPrompt["engine"]) => {
      window.localStorage.setItem(
        engine === "codex" ? CODEX_DISMISSED_KEY : OPENCODE_DISMISSED_KEY,
        "true",
      );
      setPrompt(engine, null);
    },
    [setPrompt],
  );

  const enableHooks = useCallback(
    async (engine: HookPrompt["engine"]) => {
      const idle = { onEnable: () => {}, onDismiss: () => {} };
      setPrompt(engine, {
        engine,
        offer: "",
        state: "working",
        message: null,
        ...idle,
      });
      let message: string;
      try {
        const report =
          engine === "codex" ? await api.codexHooksEnable() : await api.opencodeHooksEnable();
        message = report.message;
      } catch {
        message =
          engine === "codex"
            ? "Codex did not accept the change."
            : "OpenCode did not accept the change.";
      }
      setPrompt(engine, {
        engine,
        offer: "",
        state: "answered",
        message,
        ...idle,
      });
      messageTimer.current = window.setTimeout(() => setPrompt(engine, null), HOOKS_MESSAGE_MS);
    },
    [api, setPrompt],
  );

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

  // The waiting-on-you offers, asked once per window.
  //
  // **A failure here is silent on purpose.** This is an optional capability, not a session read:
  // a host that cannot answer it, or an owner who already said no, must not be shown a problem
  // notice about it. Nothing about the live view depends on the answer.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const offers: {
        engine: HookPrompt["engine"];
        offer: string;
        dismissedKey: string;
        installed(): Promise<{ installed: boolean }>;
      }[] = [
        {
          engine: "codex",
          offer: "Pigeon can show when Codex waits on you.",
          dismissedKey: CODEX_DISMISSED_KEY,
          installed: () => api.codexHooksStatus(),
        },
        {
          engine: "opencode",
          offer: "Pigeon can show when OpenCode waits on you.",
          dismissedKey: OPENCODE_DISMISSED_KEY,
          installed: () => api.opencodeHooksStatus(),
        },
      ];
      for (const candidate of offers) {
        try {
          const { installed } = await candidate.installed();
          if (cancelled || installed) continue;
          if (window.localStorage.getItem(candidate.dismissedKey) === "true") continue;
          setPrompt(candidate.engine, {
            engine: candidate.engine,
            offer: candidate.offer,
            state: "asking",
            message: null,
            onEnable: () => void enableHooks(candidate.engine),
            onDismiss: () => dismissHooks(candidate.engine),
          });
        } catch {
          /* no offer, no complaint */
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [api, dismissHooks, enableHooks, setPrompt]);

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
      hookPrompts={hookPrompts}
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
