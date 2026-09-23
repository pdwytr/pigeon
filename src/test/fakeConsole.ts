// Test doubles for the console surface. Ported from demo-studio's `app/src/test/fakeConsole.ts`
// and re-pointed at Pigeon's `ConsoleApi`.
//
// Two fakes, and between them they are the reason no test here spawns a PTY or a real engine:
//
//   `FakeTerminal`     stands in for xterm, which cannot be opened under jsdom at all — see
//                      `console/terminalEngine.ts` for the measured failure and why the seam exists.
//   `FakeConsoleApi`   stands in for the host: a scripted console registry that records every
//                      command it was sent and can be made to emit output or exit on cue.
//
// Deliberately NOT `api/fake.ts`'s console half, which gates output on `console_ready` exactly as
// the real host does. That gate is right for the app and wrong for a test about ORDERING: a fake
// that cannot deliver a byte early can never prove the View queues one correctly. This one delivers
// whenever it is told to.

import { encodeB64 } from "../api/encoding";
import type { ConsoleApi, Unsubscribe } from "../api/types";
import type { ConsoleScrollback, ConsoleSummary, ProviderId, SessionKey } from "../bindings";
import type {
  TerminalFactory,
  TerminalHandle,
  TerminalOptions,
  TerminalSize,
} from "../console/terminalEngine";

/** Every command the api was asked to perform, in order — so a test can assert not just THAT
 *  `console_ready` happened but that it happened last. */
export type ConsoleCall =
  | { kind: "ready"; id: string }
  | { kind: "input"; id: string; dataB64: string }
  | { kind: "resize"; id: string; cols: number; rows: number }
  | { kind: "close"; id: string }
  | { kind: "list" }
  | { kind: "scrollback"; id: string };

export class FakeConsoleApi implements ConsoleApi {
  calls: ConsoleCall[] = [];
  rows: ConsoleSummary[] = [];
  /** What `console_scrollback` returns, per console id. */
  scrollbackText = new Map<string, string>();
  scrollbackTruncated = false;
  /** Set to make `console_list` reject, which is how the "host cannot answer" path is reached. */
  listRejects = false;
  /** Set to make `console_resize` reject, for the non-blocking-notice path. */
  resizeRejects = false;
  /** Set to make `console_input` reject, which is what a keystroke to a dead id really does. */
  inputRejects = false;
  /** The call log as it stood the moment `console_ready` was sent. The ordering rule is "ready
   *  only after everything else", and a call count alone cannot tell you whether that held. */
  callsAtReady: ConsoleCall["kind"][] | null = null;
  private dataSubs = new Set<(e: { id: string; dataB64: string }) => void>();
  private exitSubs = new Set<(e: { id: string; exitCode: number | null }) => void>();

  constructor(rows: ConsoleSummary[] = []) {
    this.rows = rows;
  }

  /** A running console row, with the fields a test does not care about filled in. */
  static running(
    id: string,
    sessionKey: SessionKey | null = null,
    scrollbackBytes = 0,
    provider: ProviderId = "claude-code",
  ): ConsoleSummary {
    return {
      id,
      sessionKey,
      provider,
      cwd: "/Users/khalid/Documents/Projects/pigeon",
      mode: sessionKey ? "resume" : "new",
      state: "running",
      cols: 80,
      rows: 24,
      scrollbackBytes,
      exitCode: null,
      startedAtMs: 1_760_000_000_000,
    };
  }

  static ended(id: string, exitCode: number | null): ConsoleSummary {
    return { ...FakeConsoleApi.running(id), state: "exited", exitCode };
  }

  async consoleReady(args: { id: string }): Promise<void> {
    this.callsAtReady = this.calls.map((c) => c.kind);
    this.calls.push({ kind: "ready", id: args.id });
  }
  async consoleInput(args: { id: string; dataB64: string }): Promise<void> {
    this.calls.push({ kind: "input", ...args });
    if (this.inputRejects) throw new Error("no console with that id");
  }
  async consoleResize(args: { id: string; cols: number; rows: number }): Promise<void> {
    this.calls.push({ kind: "resize", ...args });
    if (this.resizeRejects) throw new Error("host refused the resize");
  }
  async consoleClose(args: { id: string }): Promise<void> {
    this.calls.push({ kind: "close", id: args.id });
  }
  async consoleList(): Promise<{ consoles: ConsoleSummary[] }> {
    this.calls.push({ kind: "list" });
    if (this.listRejects) throw new Error("host unreachable");
    return { consoles: this.rows.map((r) => ({ ...r })) };
  }
  async consoleScrollback(args: { id: string }): Promise<ConsoleScrollback> {
    this.calls.push({ kind: "scrollback", id: args.id });
    return {
      id: args.id,
      dataB64: encodeB64(this.scrollbackText.get(args.id) ?? ""),
      truncated: this.scrollbackTruncated,
    };
  }
  onConsoleData(cb: (e: { id: string; dataB64: string }) => void): Unsubscribe {
    this.dataSubs.add(cb);
    return () => void this.dataSubs.delete(cb);
  }
  onConsoleExit(cb: (e: { id: string; exitCode: number | null }) => void): Unsubscribe {
    this.exitSubs.add(cb);
    return () => void this.exitSubs.delete(cb);
  }

  // -- the scripting side -------------------------------------------------------------------

  /** Push a live chunk to every subscriber, ungated. */
  emit(id: string, text: string): void {
    for (const cb of [...this.dataSubs]) cb({ id, dataB64: encodeB64(text) });
  }
  emitExit(id: string, exitCode: number | null): void {
    for (const cb of [...this.exitSubs]) cb({ id, exitCode });
  }
  /** How many subscribers are live — the check that a torn-down view really unsubscribed. */
  get subscriberCount(): number {
    return this.dataSubs.size + this.exitSubs.size;
  }
  kinds(): ConsoleCall["kind"][] {
    return this.calls.map((c) => c.kind);
  }
}

/** A terminal engine that records instead of rendering. */
export class FakeTerminal implements TerminalHandle {
  written: Uint8Array[] = [];
  lines: string[] = [];
  opened: HTMLElement | null = null;
  readOnly = false;
  focused = false;
  disposed = false;
  options: TerminalOptions;
  /** What the next `fit()` resolves to. `null` models a container with no layout yet. */
  nextFit: TerminalSize | null = { cols: 80, rows: 24 };
  size: TerminalSize = { cols: 80, rows: 24 };
  private dataCb: ((d: string) => void) | null = null;

  constructor(options: TerminalOptions) {
    this.options = options;
  }

  open(el: HTMLElement): void {
    this.opened = el;
  }
  write(bytes: Uint8Array): void {
    this.written.push(bytes);
  }
  writeln(text: string): void {
    this.lines.push(text);
  }
  onData(cb: (data: string) => void): void {
    this.dataCb = cb;
  }
  fit(): TerminalSize | null {
    if (this.nextFit) this.size = this.nextFit;
    return this.nextFit;
  }
  setReadOnly(readOnly: boolean): void {
    this.readOnly = readOnly;
  }
  focus(): void {
    this.focused = true;
  }
  dispose(): void {
    this.disposed = true;
  }

  /** Type at the terminal, as xterm would report it. */
  type(data: string): void {
    this.dataCb?.(data);
  }
  /** Everything written, decoded — one string, so a chunk boundary is not something every
   *  assertion has to know about. */
  get text(): string {
    const total = this.written.reduce((n, c) => n + c.length, 0);
    const flat = new Uint8Array(total);
    let at = 0;
    for (const chunk of this.written) {
      flat.set(chunk, at);
      at += chunk.length;
    }
    return new TextDecoder().decode(flat);
  }
}

/**
 * A factory plus a handle on whatever it built. The factory is async like the real one, so the
 * component's await-then-attach path is the one under test rather than a synchronous shortcut.
 *
 * `initialFit` is what the terminal reports the FIRST time it is fitted, and it has to be settable
 * here rather than on the returned handle: `ConsoleView` fits and reports the geometry during the
 * attach, which is over before a test that waited for the handle could touch it.
 */
export function fakeTerminalFactory(initialFit: TerminalSize | null = { cols: 80, rows: 24 }): {
  factory: TerminalFactory;
  get: () => FakeTerminal | null;
} {
  let made: FakeTerminal | null = null;
  return {
    factory: async (opts) => {
      made = new FakeTerminal(opts);
      made.nextFit = initialFit;
      return made;
    },
    get: () => made,
  };
}

/**
 * A factory that does not hand back its terminal until `release()` is called.
 *
 * The attach has real awaits in it — the engine's dynamic import, `document.fonts.ready`, the
 * resize, the list, the scrollback — and several bugs live in exactly that window: an exit that
 * arrives mid-attach, a panel switched to another console before the first one finished. A gate is
 * the only way to hold a test inside that window deterministically.
 */
export function gatedTerminalFactory(initialFit: TerminalSize | null = { cols: 80, rows: 24 }): {
  factory: TerminalFactory;
  release: () => void;
  get: () => FakeTerminal | null;
} {
  let made: FakeTerminal | null = null;
  let open: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    open = resolve;
  });
  return {
    factory: async (opts) => {
      await gate;
      made = new FakeTerminal(opts);
      made.nextFit = initialFit;
      return made;
    },
    release: () => open(),
    get: () => made,
  };
}

/**
 * Install a `ResizeObserver` that a test can fire by hand, and return the trigger.
 *
 * jsdom implements none, so `ConsoleView` skips the observer entirely under test unless one is
 * provided. Call the returned `restore` in a `finally`/`afterEach`, or every later test in the file
 * inherits it.
 */
export function captureResizeObserver(): { fire: () => void; restore: () => void } {
  const previous = globalThis.ResizeObserver;
  let cb: (() => void) | null = null;
  class Capturing {
    constructor(callback: () => void) {
      cb = callback;
    }
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  globalThis.ResizeObserver = Capturing as unknown as typeof ResizeObserver;
  return {
    fire: () => cb?.(),
    restore: () => {
      globalThis.ResizeObserver = previous;
    },
  };
}
