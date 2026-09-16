// One console, rendered.
//
// The terminal for a single console id: it decodes that id's `console://data` chunks into the grid,
// encodes keystrokes back through `console_input`, keeps the host's PTY the same size as the
// viewport, and says in words when the process behind it has ended.
//
// Ported from demo-studio's `app/src/console/ConsoleView.tsx`, with one addition Pigeon's
// contract requires and studio had no equivalent of: the SCROLLBACK REPLAY of a detached console.
//
// **It holds no shadow copy of the console.** The three things this component keeps in state —
// attached / ended / gone, and an exit code — are all facts the host told it, either in
// `console_list`'s answer or in a `console://exit` event. Nothing here infers a console's state
// from the absence of output.
//
// -- The attach order, which is frozen ------------------------------------------------------------
//
//     subscribe console://data and console://exit
//     -> create the terminal
//     -> fit            (after `document.fonts.ready`)
//     -> console_resize
//     -> console_list
//     -> console_scrollback   (only for an existing detached console)
//     -> console_ready        LAST
//
// Four of those positions are load-bearing and none of them is arbitrary:
//
// 1. **Subscribe BEFORE asking `console_list`.** The other order has a window in which output lands
//    between the answer and the subscription and is simply lost. A redundant subscription costs
//    nothing; a dropped chunk is a hole in the middle of the scrollback with nothing to mark it.
//
// 2. **Buffer output that arrives before the terminal exists, AND before the replay is written.**
//    Building the engine means a dynamic import, so there are real milliseconds between subscribing
//    and having anything to write to. Chunks queue, and — this is the part studio did not need —
//    they stay queued until after `console_scrollback`'s bytes are in, because a live byte written
//    ahead of the replay would appear ABOVE the history it came after.
//
// 3. **Fit only after the fonts have settled.** `FitAddon` measures a character cell; measured
//    against a fallback face the cell is the wrong width and the grid comes up the wrong size
//    (demo-studio backlog #114).
//
// 4. **`console_ready` is the LAST step, never an earlier one.** The host holds a console's output
//    back until it hears this, because a fresh pseudoconsole child suspends on a DSR (`ESC[6n`)
//    query until an attached terminal answers it. Readying too early releases that query into a
//    View that cannot yet reply, which leaves the child wedged just as thoroughly as never readying
//    at all. See `announceReady`.
//
// -- The exited state is WORDS, not a dead prompt --------------------------------------------------
//
// A terminal whose process has ended looks exactly like a terminal waiting for the process to say
// something; the only difference is that one of them will never respond. So the state is stated: a
// notice above the grid naming what happened and the code it happened with, and the grid goes
// read-only so the cursor stops inviting input it cannot deliver. Colour never carries it alone — a
// non-zero code is tinted AND spelled out.

import { useCallback, useEffect, useRef, useState } from "react";
import { decodeB64, encodeB64 } from "../api/encoding";
import type { ConsoleApi } from "../api/types";
import type { ConsoleSummary } from "../bindings";
import { terminalFontFamily, terminalPalette } from "./consoleTheme";
import {
  createXtermTerminal,
  fontsReady,
  type TerminalFactory,
  type TerminalHandle,
} from "./terminalEngine";

/** What this view knows about its console. `gone` is a real answer, not an error state: a console
 *  can end and be reaped before its panel is opened, and saying so is more use than an empty grid. */
export type ConsolePhase = "attaching" | "live" | "exited" | "gone";

export interface ConsoleViewState {
  phase: ConsolePhase;
  exitCode: number | null;
  summary: ConsoleSummary | null;
  /** True while the bounded scrollback of a detached console is being restored. */
  replaying: boolean;
  truncated: boolean;
}

export interface ConsoleViewProps {
  id: string;
  api: ConsoleApi;
  /** Injected in tests; production builds a real xterm. See `terminalEngine.ts` for why this seam
   *  exists (jsdom cannot open one — measured, not assumed). */
  createTerminal?: TerminalFactory;
  /** From `host_info`. Decides ConPTY line handling and nothing else. */
  hostOs?: string | null;
  /** Replay this console's bounded scrollback before going live. True when reattaching to a console
   *  that was already running, false for one this pane just opened. */
  replay?: boolean;
  /** Provider, project and session, for assistive technology. */
  label: string;
  fontSize?: number;
  onStateChange?: (state: ConsoleViewState) => void;
  /** A terminal input happened; the host decides whether it changed the agent's turn state. */
  onActivity?: () => void;
}

/**
 * What the notice says about an ended process. One function so the wording is pinned by a test
 * rather than assembled inline in JSX.
 *
 * `null` is not "code 0" and must never be printed as one: it is what the host reports for a
 * process that was KILLED — by `console_close`, or by the OS — and telling the owner their console
 * "exited normally (0)" when it was killed mid-turn would be a plain falsehood.
 */
export function exitWording(code: number | null): {
  headline: string;
  detail: string;
  bad: boolean;
} {
  if (code === null) {
    return {
      headline: "Process ended",
      detail: "ended without an exit code — it was closed or killed",
      bad: false,
    };
  }
  if (code === 0) return { headline: "Process ended", detail: "exit code 0", bad: false };
  return { headline: "Process ended", detail: `exit code ${code}`, bad: true };
}

export function ConsoleView({
  id,
  api,
  createTerminal = createXtermTerminal,
  hostOs = null,
  replay = false,
  label,
  fontSize = 13,
  onStateChange,
  onActivity,
}: ConsoleViewProps) {
  const hostRef = useRef<HTMLElement | null>(null);
  const termRef = useRef<TerminalHandle | null>(null);
  const [phase, setPhase] = useState<ConsolePhase>("attaching");
  const [exitCode, setExitCode] = useState<number | null>(null);
  const [summary, setSummary] = useState<ConsoleSummary | null>(null);
  const [replaying, setReplaying] = useState(replay);
  const [truncated, setTruncated] = useState(false);
  const [resizeProblem, setResizeProblem] = useState<string | null>(null);

  // The live phase as a REF as well as state: the keystroke handler is registered once against the
  // engine and would otherwise close over the phase it was created in, and go on posting input to a
  // console that has since ended.
  const liveRef = useRef(false);
  // Has THIS attach already heard the console end? The attach is a sequence of awaits — the engine
  // import, `document.fonts.ready` (up to 1.5 s), the resize, the list, the scrollback — and a
  // `console://exit` can land in the middle of it. `console_list` then answers from a snapshot
  // taken before the process died, and the live branch below would overwrite the exit with
  // "running": the notice disappears, the gate reopens, and keystrokes go to a dead id.
  const endedRef = useRef(false);
  // The last geometry the host was told about, so a resize observer firing every frame of a drag
  // sends one command per actual grid change rather than one per frame.
  const sentSizeRef = useRef<string>("");

  useEffect(() => {
    onStateChange?.({ phase, exitCode, summary, replaying, truncated });
  }, [phase, exitCode, summary, replaying, truncated, onStateChange]);

  useEffect(() => {
    let cancelled = false;
    // A fresh attach, and nothing it has not heard for itself. Set before the subscriptions below,
    // so no exit can be missed between the two.
    endedRef.current = false;
    // Chunks that arrive before the engine is built, or before the replay has been written. See
    // ordering notes (2) in the header.
    const queued: Uint8Array[] = [];
    let flushed = false;

    const writeOut = (bytes: Uint8Array) => {
      const term = termRef.current;
      if (term && flushed) term.write(bytes);
      else queued.push(bytes);
    };

    const flush = () => {
      flushed = true;
      const term = termRef.current;
      if (!term) return;
      for (const chunk of queued.splice(0)) term.write(chunk);
    };

    const markExited = (code: number | null) => {
      liveRef.current = false;
      endedRef.current = true;
      setExitCode(code);
      setPhase("exited");
      termRef.current?.setReadOnly(true);
    };

    /**
     * Tell the host this View is listening — the last step of an attach, never an earlier one.
     *
     * The contract requires only "after the listeners are registered"; this deliberately goes
     * further and waits until the attach is COMPLETE. The reason is the return leg of the
     * handshake. The host holds output back until this call; what then arrives first is the child's
     * DSR query, and the thing that un-wedges the child is xterm's ANSWER to it — which travels
     * back out through `term.onData` and `console_input`, i.e. through the very path this component
     * gates on `liveRef`. Readying while that gate was still shut would release the query into a
     * View that drops the reply, leaving the child suspended exactly as if nothing had been readied
     * at all — the same wedge, now with a green-looking handshake in front of it.
     *
     * A failure is swallowed rather than promoted to `gone`. The honest reading of a rejection here
     * is "the console vanished between `console_list` and now", which is a real but rare race; the
     * visible consequence is a console that prints nothing, and the alternative — contradicting the
     * list answer we just got — is a worse guess than saying nothing.
     */
    const announceReady = async () => {
      if (cancelled) return;
      try {
        await api.consoleReady({ id });
      } catch {
        /* see above */
      }
    };

    // (1) Subscriptions first, before any question is asked of the host.
    const stopData = api.onConsoleData((e) => {
      if (cancelled || e.id !== id) return;
      writeOut(decodeB64(e.dataB64));
    });
    const stopExit = api.onConsoleExit((e) => {
      if (cancelled || e.id !== id) return;
      markExited(e.exitCode);
    });

    (async () => {
      const term = await createTerminal({
        palette: terminalPalette(),
        fontFamily: terminalFontFamily(),
        fontSize,
        windowsPty: hostOs === "windows",
      });
      if (cancelled || !hostRef.current) {
        term.dispose();
        return;
      }
      termRef.current = term;
      term.open(hostRef.current);
      term.onData((data) => {
        // A console that has ended swallows keystrokes rather than posting them at a dead id. The
        // engine is read-only by then too; this is the belt to that braces, because `setReadOnly`
        // is the engine's promise and this is ours.
        if (!liveRef.current) return;
        onActivity?.();
        // A rejection here is a console the host no longer holds — the id died between the gate
        // above and this call. Nothing to show and nothing to retry, but it has to be CAUGHT: an
        // uncaught one is an unhandled rejection per keystroke.
        void api.consoleInput({ id, dataB64: encodeB64(data) }).catch(() => {});
      });

      // (3) Measure only once the real face is loaded.
      await fontsReady();
      if (cancelled) return;

      // The grid's real geometry, and the host is told about it immediately: the console was opened
      // with whatever size the LAUNCHER guessed, and this pane is the first thing that knows how
      // big the terminal actually is. Skipping this leaves a PTY whose idea of the width is wrong,
      // which is invisible until something wraps.
      const size = term.fit();
      if (size) {
        sentSizeRef.current = `${size.cols}x${size.rows}`;
        try {
          await api.consoleResize({ id, cols: size.cols, rows: size.rows });
          if (!cancelled) setResizeProblem(null);
        } catch {
          // Non-blocking by contract: a resize failure must never destroy the renderer.
          if (!cancelled) setResizeProblem("The host did not accept the terminal size.");
        }
      }
      if (cancelled) return;

      // (…and only now ask what this console IS. Subscriptions have been live since before the
      // engine was built, so nothing between then and here was dropped.)
      let rows: ConsoleSummary[];
      try {
        rows = (await api.consoleList()).consoles;
      } catch {
        // A host that cannot answer is not a console that has ended. Say the honest thing rather
        // than inventing an exit code.
        if (!cancelled) setPhase("gone");
        return;
      }
      if (cancelled) return;
      const mine = rows.find((r) => r.id === id) ?? null;
      setSummary(mine);
      if (!mine) {
        setPhase("gone");
        term.setReadOnly(true);
        flush();
        return;
      }

      // Exactly once per attach, and only for a console that was already running when this pane
      // adopted it. The bytes go in BEFORE the queued live chunks, which is the whole ordering.
      if (replay && mine.scrollbackBytes > 0) {
        try {
          const restored = await api.consoleScrollback({ id });
          if (cancelled) return;
          term.write(decodeB64(restored.dataB64));
          setTruncated(restored.truncated);
        } catch {
          // A scrollback the host cannot produce is a gap in the history, not a broken console.
          if (!cancelled) setTruncated(true);
        }
      }
      setReplaying(false);
      flush();

      // An exit this view heard for itself outranks the list's answer: the event is newer than
      // the snapshot the list was built from, whatever that snapshot says.
      if (endedRef.current) {
        // Readied anyway, for the same reason as below: the host may be holding the console's
        // final output behind the gate, and those are the words most worth reading.
        await announceReady();
        return;
      }

      const ended = mine.state === "exited" || mine.state === "closed";
      if (ended) {
        // Ended before this pane ever opened — a `console://exit` this view was never around to
        // hear. The reattach path has to carry it, or the grid would sit there looking live.
        markExited(mine.exitCode);
        // Readied anyway: the console is PRESENT, so the host may be holding its last output behind
        // the same gate, and a dead console's final words are the ones most worth reading.
        await announceReady();
        return;
      }
      liveRef.current = true;
      setPhase("live");
      await announceReady();
    })().catch(() => {
      // A console that cannot be attached must not take down the pane it lives in; the notice below
      // is what the owner sees instead of a blank rectangle.
      if (!cancelled) setPhase("gone");
    });

    return () => {
      // Detaching. Note what is NOT here: `console_close`. Selecting another session takes the
      // renderer down and leaves the host's PTY running, which is what makes coming back cheap.
      cancelled = true;
      liveRef.current = false;
      stopData();
      stopExit();
      termRef.current?.dispose();
      termRef.current = null;
    };
  }, [id, api, createTerminal, fontSize, hostOs, replay, onActivity]);

  // The viewport's size is the PTY's size. `ResizeObserver` rather than a window resize listener:
  // this element is resized by things a window event never fires for — the notice appearing above
  // it being the obvious one, since that changes the grid's height without the window moving.
  const onResize = useCallback(() => {
    const term = termRef.current;
    if (!term) return;
    const size = term.fit();
    if (!size) return;
    const key = `${size.cols}x${size.rows}`;
    if (key === sentSizeRef.current) return;
    sentSizeRef.current = key;
    api
      .consoleResize({ id, cols: size.cols, rows: size.rows })
      // Cleared on success: one transient failure used to leave "Size not applied" on screen for
      // the life of the panel, long after the host had accepted every size since.
      .then(() => setResizeProblem(null))
      .catch(() => setResizeProblem("The host did not accept the terminal size."));
  }, [id, api]);

  useEffect(() => {
    const el = hostRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => onResize());
    ro.observe(el);
    return () => ro.disconnect();
  }, [onResize]);

  const wording = phase === "exited" ? exitWording(exitCode) : null;

  return (
    <div data-testid="console-view" data-phase={phase}>
      {wording && (
        <div className="terminal-note" data-testid="console-exit-notice">
          <b>{wording.headline}</b>
          {/* The code is tinted when it is a failure AND spelled out either way — colour is never
              the only carrier of a state. */}
          <span data-testid="console-exit-detail" className={wording.bad ? "bad" : undefined}>
            {wording.detail}
          </span>
        </div>
      )}
      {phase === "gone" && (
        <div className="terminal-note" data-testid="console-gone-notice">
          <b>Console not found</b>
          <span>the host has no console {id} — it ended and was reaped, or the app restarted</span>
        </div>
      )}
      {replaying && (
        <div className="terminal-note" data-testid="console-replay-notice">
          <b>Restoring</b>
          <span>replaying this console's recent output…</span>
        </div>
      )}
      {truncated && !replaying && (
        <div className="terminal-note" data-testid="console-truncated-notice">
          <b>Partial history</b>
          <span>only the most recent output was restored</span>
        </div>
      )}
      {resizeProblem && (
        <div className="terminal-note" data-testid="console-resize-notice">
          <b>Size not applied</b>
          <span>{resizeProblem}</span>
        </div>
      )}
      {/* The grid. A named region rather than a div with a role bolted on: xterm's own textarea
          carries no context about which session's output is on screen, and a `<section>` with an
          accessible name is the element that already means "a labelled area of the page". */}
      <section
        data-testid="console-grid"
        className="terminal-grid"
        ref={hostRef}
        aria-label={label}
      />
    </div>
  );
}
