// The terminal ENGINE behind a seam, and the real xterm.js implementation of it.
//
// Ported from demo-studio's `app/src/console/terminalEngine.ts`. Its argument for the seam
// holds here unchanged, and it was measured rather than assumed:
//
// **xterm cannot be opened under jsdom, so a `ConsoleView` that constructed one directly would have
// no unit tests.** `Terminal.open()` throws:
//
//     TypeError: this._parentWindow.matchMedia is not a function
//       at a._updateDpr (src/browser/services/CoreBrowserService.ts:125)
//       at x.open       (src/browser/CoreBrowserTerminal.ts:457)
//
// preceded by jsdom's own `Not implemented: HTMLCanvasElement's getContext()`. Stubbing those two
// would not fix the interesting half either: `FitAddon.fit()` sizes the grid by MEASURING a
// character cell, and jsdom has no layout engine, so every measurement comes back 0 and the fit
// would report a garbage geometry that the test would then be asserting.
//
// So the split follows the evidence: the seam is what vitest drives, and the real engine is proved
// in a browser that has layout, a canvas and a device pixel ratio.
//
// Nine methods, no passthrough to the underlying `Terminal`. A seam that leaked the xterm object
// would let a component reach past it, and the first time one did, the unit tests would stop being
// evidence about that component.

import type { TerminalPalette } from "./consoleTheme";

/** The grid geometry a fit resolved to. */
export interface TerminalSize {
  cols: number;
  rows: number;
}

export interface TerminalHandle {
  /** Attach to a DOM element and start rendering. */
  open(el: HTMLElement): void;
  /** Write raw output bytes. Bytes, never a string — see `decodeB64`'s note on split codepoints. */
  write(bytes: Uint8Array): void;
  /** Write one line of Pigeon's OWN text (a banner, a notice) into the scrollback. */
  writeln(text: string): void;
  /** Keystrokes, already encoded by xterm into the bytes a PTY expects. */
  onData(cb: (data: string) => void): void;
  /** Re-fit the grid to the element. Returns the new size, or `null` if it could not be measured
   *  (a detached or zero-sized container — a normal transient, not an error). */
  fit(): TerminalSize | null;
  /** Stop accepting keystrokes. A console whose process has ended must not look typeable. */
  setReadOnly(readOnly: boolean): void;
  focus(): void;
  dispose(): void;
  readonly size: TerminalSize;
}

export interface TerminalOptions {
  palette: TerminalPalette;
  fontFamily: string;
  fontSize: number;
  /**
   * ConPTY line handling, and the one line of this file that is NOT studio's.
   *
   * Studio pins `windowsPty` on unconditionally because studio only ships for Windows. Pigeon runs
   * on macOS first, and xterm's `windowsPty` mode changes how wrapped lines are reflowed to suit a
   * ConPTY's output; switching it on over a Unix pty makes a resize interleave old output instead
   * of fixing it. So the host's own `host_info.os` decides, and nothing guesses from the user agent.
   */
  windowsPty: boolean;
}

/** Asynchronous because the real one dynamic-imports xterm — what keeps a ~400 KB terminal library
 *  out of every unrelated vitest file's module graph. */
export type TerminalFactory = (opts: TerminalOptions) => Promise<TerminalHandle>;

/** The real engine. */
export const createXtermTerminal: TerminalFactory = async (opts) => {
  const [{ Terminal }, { FitAddon }] = await Promise.all([
    import("@xterm/xterm"),
    import("@xterm/addon-fit"),
  ]);
  // The library's own stylesheet, imported here and nowhere else: it is the engine's dependency,
  // not the component's, so a surface that never opens a terminal never pays for it.
  await import("@xterm/xterm/css/xterm.css");

  const term = new Terminal({
    fontFamily: opts.fontFamily,
    fontSize: opts.fontSize,
    theme: opts.palette,
    // A cursor that blinks is how a terminal says "you can type here", and this pane's whole
    // purpose is that you can.
    cursorBlink: true,
    // 5,000 lines rather than the 1,000 default: an agent run prints a lot, and scrollback that
    // silently drops the start of a session is scrollback that lies about what happened.
    scrollback: 5000,
    ...(opts.windowsPty ? { windowsPty: { backend: "conpty" as const } } : {}),
    allowProposedApi: false,
  });
  const fitAddon = new FitAddon();
  term.loadAddon(fitAddon);

  return {
    open(el) {
      term.open(el);
    },
    write(bytes) {
      term.write(bytes);
    },
    writeln(text) {
      term.writeln(text);
    },
    onData(cb) {
      term.onData(cb);
    },
    fit() {
      // `proposeDimensions` measures before committing, so a container that is not laid out yet
      // (0x0 during a mount, or hidden) is a `null` rather than a `fit()` that throws or snaps the
      // grid to 1x1 and then has to be undone.
      const proposed = fitAddon.proposeDimensions();
      if (!proposed || !Number.isFinite(proposed.cols) || !Number.isFinite(proposed.rows))
        return null;
      if (proposed.cols < 1 || proposed.rows < 1) return null;
      fitAddon.fit();
      return { cols: term.cols, rows: term.rows };
    },
    setReadOnly(readOnly) {
      term.options.disableStdin = readOnly;
      term.options.cursorBlink = !readOnly;
    },
    focus() {
      term.focus();
    },
    dispose() {
      term.dispose();
    },
    get size() {
      return { cols: term.cols, rows: term.rows };
    },
  };
};

/**
 * Wait for web fonts before the first `fit()`.
 *
 * `FitAddon` derives the grid from a measured character cell. Measure while the browser is still
 * painting a fallback face and the cell is the wrong width, so the terminal comes up with the wrong
 * number of columns and only a later resize corrects it — demo-studio backlog #114.
 *
 * Guarded twice over: jsdom has no `document.fonts` at all, and a `ready` that never settles must
 * not hold the attach open forever, so the wait is capped.
 */
export async function fontsReady(timeoutMs = 1500): Promise<void> {
  const fonts = typeof document === "undefined" ? undefined : document.fonts;
  if (!fonts?.ready) return;
  await Promise.race([
    fonts.ready.then(() => undefined),
    new Promise<void>((resolve) => setTimeout(resolve, timeoutMs)),
  ]).catch(() => undefined);
}
