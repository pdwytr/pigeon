import { getCurrentWindow } from "@tauri-apps/api/window";
import { useEffect, useState } from "react";
import type { AccountStatus, SessionRow, StatusSnapshot } from "../bindings";
import { AccountsStrip } from "./CapacityMeter";
import { LiveSessionRoster } from "./LiveSessionRoster";

export interface LiveDockProps {
  status: StatusSnapshot | null;
  sessions: SessionRow[];
  accounts: Partial<Record<AccountStatus["provider"], AccountStatus>>;
  loading: boolean;
  problem: string | null;
  /** The Codex waiting-on-you offer, when there is one to make. */
  hookPrompt?: HookPrompt | null;
}

/**
 * The one owner decision this app asks for: may Pigeon see when Codex is waiting on you?
 *
 * It is an offer rather than an error, so it never hides a session or blocks the window. `answer`
 * is what Pigeon's own command said, so a refused install is reported rather than assumed.
 */
export interface HookPrompt {
  state: "asking" | "working" | "answered";
  message: string | null;
  onEnable(): void;
  onDismiss(): void;
}

/** A compact, data-first live session window. */
export function LiveDock({
  status,
  sessions,
  accounts,
  loading,
  problem,
  hookPrompt,
}: LiveDockProps) {
  const [limitsOpen, setLimitsOpen] = useState(false);
  const [dragging, setDragging] = useState(false);

  // `:active` cannot carry the grabbing cursor here. `startDragging()` hands the gesture to the
  // window server, and WKWebView delivers no further mouse events for its duration — including the
  // mouseup that ends it — so the header would latch on the first drag and never let go. The state
  // is cleared from window-level listeners instead, with `blur` as the backstop for a drag that
  // finishes with the pointer outside the webview.
  useEffect(() => {
    if (!dragging) return;
    const stop = () => setDragging(false);
    window.addEventListener("mouseup", stop);
    window.addEventListener("blur", stop);
    return () => {
      window.removeEventListener("mouseup", stop);
      window.removeEventListener("blur", stop);
    };
  }, [dragging]);

  return (
    <div className="hover-backdrop window">
      <main className="dock-stage" aria-label="Pigeon live activity" data-testid="live-hover">
        <div className={`rig${limitsOpen ? " limits-open" : ""}`}>
          <div className="dock-content">
            {/* biome-ignore lint/a11y/noStaticElementInteractions: the header is the native window drag surface */}
            <header
              className={dragging ? "plate-header dragging" : "plate-header"}
              data-tauri-drag-region
              data-testid="hover-header"
              onMouseDown={(event) => {
                if (event.target instanceof Element && event.target.closest("button")) return;
                setDragging(true);
                void getCurrentWindow().startDragging();
              }}
            >
              <button
                type="button"
                className="plate-close plate-minimize"
                aria-label="Minimize hover"
                onClick={() => void getCurrentWindow().minimize()}
              >
                −
              </button>
              <span>PIGEON</span>
              {status ? (
                <span className="dock-status">
                  <span className="dock-open-label">
                    <strong>{sessions.length}</strong> open agent
                  </span>
                  <span className="dock-running-count">
                    {sessions.filter((session) => session.status === "running").length} running
                  </span>
                </span>
              ) : null}
              <button
                type="button"
                className="limits-toggle"
                aria-expanded={limitsOpen}
                onClick={() => setLimitsOpen((open) => !open)}
              >
                {limitsOpen ? "← AGENTS" : "USAGE LIMITS"}
              </button>
            </header>
            {limitsOpen ? (
              <aside className="limits-panel" aria-label="Provider limits">
                <AccountsStrip accounts={accounts} />
              </aside>
            ) : (
              <div className="dock-wing dock-wing-sessions">
                {hookPrompt ? (
                  <div className="dock-hook-prompt" data-testid="hook-prompt">
                    {hookPrompt.state === "asking" ? (
                      <>
                        <span>Pigeon can show when Codex waits on you.</span>
                        <button type="button" onClick={hookPrompt.onEnable}>
                          Yes
                        </button>
                        <button type="button" onClick={hookPrompt.onDismiss}>
                          Not now
                        </button>
                      </>
                    ) : (
                      <span data-testid="hook-prompt-message">
                        {hookPrompt.state === "working"
                          ? "Setting up…"
                          : (hookPrompt.message ?? "Done.")}
                      </span>
                    )}
                  </div>
                ) : null}
                {problem ? (
                  <p className="dock-message" role="status" data-testid="hover-problem">
                    {problem}
                  </p>
                ) : loading && !status ? (
                  <div className="dock-loading" data-testid="hover-skeleton" aria-busy="true">
                    <span />
                    <span />
                    <span className="sr-only">Reading live sessions</span>
                  </div>
                ) : sessions.length === 0 ? (
                  <p className="dock-message">No live sessions.</p>
                ) : (
                  <LiveSessionRoster sessions={sessions} />
                )}
              </div>
            )}
          </div>
        </div>
      </main>
    </div>
  );
}
