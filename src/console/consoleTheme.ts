// The terminal's colours, read off the house tokens at render time.
//
// Ported from demo-studio's `app/src/console/consoleTheme.ts`, with its `readToken` import from
// `charts/theme.ts` inlined — Pigeon has no chart layer to borrow it from, and one six-line
// function is not worth a module.
//
// **xterm ships its own fixed dark palette and will use it if nobody says otherwise.** That is the
// failure mode this module exists to prevent: one surface in the app whose colours are unrelated to
// every other surface's.
//
// Read per build, never cached: the tokens can change under the document (a theme switch, a user
// stylesheet), and a palette captured once at module load would be the wrong one for the rest of
// the window's life.
//
// The fallbacks are the dark theme's literals. jsdom resolves no custom properties, so without them
// every terminal in a vitest run would be themed with empty strings, and xterm rejects an empty
// colour string by throwing.

/** jsdom does not resolve custom properties via `getComputedStyle`, so callers pass a fallback that
 *  keeps the palette deterministic in tests; a real browser overrides it from `styles/pigeon.css`. */
export function readToken(name: string, fallback: string): string {
  if (typeof window === "undefined") return fallback;
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return value || fallback;
}

/** xterm's `ITheme`, structurally. Declared here rather than imported from `@xterm/xterm` so this
 *  module — and every test that reads a colour off it — stays free of the terminal library and the
 *  canvas/DOM machinery it drags in on import. */
export interface TerminalPalette {
  foreground: string;
  background: string;
  cursor: string;
  cursorAccent: string;
  selectionBackground: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
}

/** `[xterm's field, the token that feeds it, the dark-theme fallback]`. One table, so a token added
 *  to the stylesheet and never mapped here is visible as an absence rather than as a colour xterm
 *  quietly defaulted. */
const PALETTE: [keyof TerminalPalette, string, string][] = [
  ["foreground", "--term-fg", "#c8d3df"],
  ["background", "--term-bg", "#080c12"],
  ["cursor", "--term-cursor", "#49d39c"],
  ["cursorAccent", "--term-cursor-text", "#080c12"],
  ["selectionBackground", "--term-selection", "rgba(105, 167, 255, 0.32)"],
  ["black", "--term-black", "#4a5666"],
  ["red", "--term-red", "#f27f80"],
  ["green", "--term-green", "#49d39c"],
  ["yellow", "--term-yellow", "#f4bd65"],
  ["blue", "--term-blue", "#69a7ff"],
  ["magenta", "--term-magenta", "#b89cff"],
  ["cyan", "--term-cyan", "#6fd2d6"],
  ["white", "--term-white", "#c8d3df"],
  ["brightBlack", "--term-bright-black", "#6d7b8d"],
  ["brightRed", "--term-bright-red", "#ff9b9c"],
  ["brightGreen", "--term-bright-green", "#6ee7b8"],
  ["brightYellow", "--term-bright-yellow", "#ffd38b"],
  ["brightBlue", "--term-bright-blue", "#8fbdff"],
  ["brightMagenta", "--term-bright-magenta", "#d2bcff"],
  ["brightCyan", "--term-bright-cyan", "#8fe3e6"],
  ["brightWhite", "--term-bright-white", "#edf2f7"],
];

/** The terminal palette resolved against whatever theme is currently in force. */
export function terminalPalette(): TerminalPalette {
  const out = {} as TerminalPalette;
  for (const [field, token, fallback] of PALETTE) out[field] = readToken(token, fallback);
  return out;
}

/** The font the terminal renders in — the app's own mono stack, so a console and a code excerpt are
 *  set in the same face. Not a token of its own: `--font-mono` already is one. */
export function terminalFontFamily(): string {
  return readToken(
    "--font-mono",
    'ui-monospace, SFMono-Regular, "Cascadia Code", Consolas, monospace',
  );
}
