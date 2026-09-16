import { render, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import {
  captureResizeObserver,
  FakeConsoleApi,
  fakeTerminalFactory,
  gatedTerminalFactory,
} from "../test/fakeConsole";
import { ConsoleView, exitWording } from "./ConsoleView";

const LABEL = "Codex terminal for Fix test suite in demo-studio";

describe("the console attach sequence", () => {
  it("calls console_ready last, and only after console_list has answered", async () => {
    const api = new FakeConsoleApi([FakeConsoleApi.running("c1")]);
    const { factory } = fakeTerminalFactory();

    render(<ConsoleView id="c1" api={api} createTerminal={factory} label={LABEL} />);

    await waitFor(() => expect(api.kinds()).toContain("ready"));
    // The host gates its output pump on `console_ready`, so anything that has to happen before the
    // first byte arrives has to happen before this call. Asserting the ORDER, not the presence.
    expect(api.callsAtReady).toEqual(["resize", "list"]);
    expect(api.kinds().at(-1)).toBe("ready");
  });

  it("sends the fitted geometry to the host before it asks what the console is", async () => {
    const api = new FakeConsoleApi([FakeConsoleApi.running("c1")]);
    const { factory } = fakeTerminalFactory({ cols: 132, rows: 43 });

    render(<ConsoleView id="c1" api={api} createTerminal={factory} label={LABEL} />);

    await waitFor(() => expect(api.kinds()).toContain("ready"));
    expect(api.calls[0]).toEqual({ kind: "resize", id: "c1", cols: 132, rows: 43 });
  });

  it("writes a detached console's scrollback before it writes any live byte", async () => {
    const row = FakeConsoleApi.running("c1", { providerId: "codex", sid: "abc" }, 64);
    const api = new FakeConsoleApi([row]);
    api.scrollbackText.set("c1", "RESTORED-HISTORY\r\n");
    const { factory, get } = fakeTerminalFactory();

    render(<ConsoleView id="c1" api={api} createTerminal={factory} replay label={LABEL} />);

    // The subscription is registered synchronously on mount, and the engine is built behind a
    // dynamic import — so this byte arrives with no terminal to write it to, which is exactly the
    // race the queue exists for.
    expect(api.subscriberCount).toBeGreaterThan(0);
    api.emit("c1", "LIVE-BYTE");

    await waitFor(() => expect(get()?.text ?? "").toContain("LIVE-BYTE"));
    const written = get()?.text ?? "";
    expect(written.indexOf("RESTORED-HISTORY")).toBeGreaterThanOrEqual(0);
    expect(written.indexOf("RESTORED-HISTORY")).toBeLessThan(written.indexOf("LIVE-BYTE"));
  });

  it("asks for a console's scrollback exactly once per attach", async () => {
    const api = new FakeConsoleApi([
      FakeConsoleApi.running("c1", { providerId: "codex", sid: "a" }, 64),
    ]);
    api.scrollbackText.set("c1", "history");
    const { factory } = fakeTerminalFactory();

    render(<ConsoleView id="c1" api={api} createTerminal={factory} replay label={LABEL} />);

    await waitFor(() => expect(api.kinds()).toContain("ready"));
    expect(api.kinds().filter((k) => k === "scrollback")).toHaveLength(1);
  });

  it("does not ask for scrollback when it opened the console itself", async () => {
    const api = new FakeConsoleApi([FakeConsoleApi.running("c1", null, 0)]);
    const { factory } = fakeTerminalFactory();

    render(<ConsoleView id="c1" api={api} createTerminal={factory} replay={false} label={LABEL} />);

    await waitFor(() => expect(api.kinds()).toContain("ready"));
    expect(api.kinds()).not.toContain("scrollback");
  });
});

describe("detaching a console", () => {
  it("releases its subscriptions and its renderer without closing the host's console", async () => {
    const api = new FakeConsoleApi([FakeConsoleApi.running("c1")]);
    const { factory, get } = fakeTerminalFactory();

    const view = render(<ConsoleView id="c1" api={api} createTerminal={factory} label={LABEL} />);
    await waitFor(() => expect(api.kinds()).toContain("ready"));

    view.unmount();

    // The PTY outlives the view. This is what makes coming back to a session cheap, and it is the
    // difference between "detach" and "close".
    expect(api.kinds()).not.toContain("close");
    expect(api.subscriberCount).toBe(0);
    expect(get()?.disposed).toBe(true);
  });
});

describe("a console whose process has ended", () => {
  it("states the exit in words and stops accepting keystrokes", async () => {
    const api = new FakeConsoleApi([FakeConsoleApi.running("c1")]);
    const { factory, get } = fakeTerminalFactory();

    const view = render(<ConsoleView id="c1" api={api} createTerminal={factory} label={LABEL} />);
    await waitFor(() => expect(api.kinds()).toContain("ready"));

    api.emitExit("c1", 2);

    const notice = await view.findByTestId("console-exit-notice");
    expect(notice).toHaveTextContent("exit code 2");
    await waitFor(() => expect(get()?.readOnly).toBe(true));

    get()?.type("still typing");
    expect(api.kinds()).not.toContain("input");
  });

  it("says a killed process ended without a code rather than calling it a clean zero", () => {
    // `null` is what the host reports for a process it killed. Printing "exit code 0" there would
    // tell the owner their run finished normally when it was cut off mid-turn.
    expect(exitWording(null).detail).toContain("without an exit code");
    expect(exitWording(null).detail).not.toContain("0");
    expect(exitWording(0).detail).toBe("exit code 0");
    expect(exitWording(1).bad).toBe(true);
  });

  it("adopts a console that had already exited before the pane opened", async () => {
    const api = new FakeConsoleApi([FakeConsoleApi.ended("c1", 0)]);
    const { factory } = fakeTerminalFactory();

    const view = render(<ConsoleView id="c1" api={api} createTerminal={factory} label={LABEL} />);

    expect(await view.findByTestId("console-exit-notice")).toHaveTextContent("exit code 0");
    // Readied anyway: the host may still be holding the console's final output behind the gate.
    expect(api.kinds()).toContain("ready");
  });
});

describe("a console the host cannot account for", () => {
  it("says so instead of showing an empty grid that looks live", async () => {
    const api = new FakeConsoleApi([]);
    const { factory } = fakeTerminalFactory();

    const view = render(
      <ConsoleView id="ghost" api={api} createTerminal={factory} label={LABEL} />,
    );

    expect(await view.findByTestId("console-gone-notice")).toBeInTheDocument();
    expect(api.kinds()).not.toContain("ready");
  });
});

describe("terminal input", () => {
  it("posts keystrokes as bytes on the console id, never on a session sid", async () => {
    const api = new FakeConsoleApi([
      FakeConsoleApi.running("c1", { providerId: "codex", sid: "sid-1" }),
    ]);
    const { factory, get } = fakeTerminalFactory();

    render(<ConsoleView id="c1" api={api} createTerminal={factory} label={LABEL} />);
    await waitFor(() => expect(api.kinds()).toContain("ready"));

    get()?.type("y\r");

    const input = api.calls.find((c) => c.kind === "input");
    expect(input).toMatchObject({ kind: "input", id: "c1" });
    // btoa("y\r")
    expect(input && "dataB64" in input ? input.dataB64 : "").toBe("eQ0=");
  });
});

describe("a console that ends while it is still attaching", () => {
  it("stays ended instead of being talked back to life by a stale console_list", async () => {
    // The attach has real awaits in it — the engine import, the fonts, the resize, the list — and
    // `console_list` answers from a snapshot taken before the process died. Letting that answer
    // set `phase: "live"` reopened the input gate and posted keystrokes at a dead id.
    const api = new FakeConsoleApi([FakeConsoleApi.running("c1")]);
    const gated = gatedTerminalFactory();

    const view = render(
      <ConsoleView id="c1" api={api} createTerminal={gated.factory} label={LABEL} />,
    );

    // Subscriptions are registered synchronously on mount, before the engine exists.
    expect(api.subscriberCount).toBeGreaterThan(0);
    api.emitExit("c1", 3);
    expect(await view.findByTestId("console-exit-notice")).toHaveTextContent("exit code 3");

    gated.release();
    await waitFor(() => expect(api.kinds()).toContain("ready"));

    expect(view.getByTestId("console-view")).toHaveAttribute("data-phase", "exited");
    expect(view.getByTestId("console-exit-detail")).toHaveTextContent("exit code 3");
    gated.get()?.type("still typing");
    expect(api.kinds()).not.toContain("input");
  });
});

describe("the viewport's size is the PTY's size", () => {
  it("sends a new geometry once per grid change, not once per observer callback", async () => {
    const observer = captureResizeObserver();
    try {
      const api = new FakeConsoleApi([FakeConsoleApi.running("c1")]);
      const { factory, get } = fakeTerminalFactory({ cols: 80, rows: 24 });

      render(<ConsoleView id="c1" api={api} createTerminal={factory} label={LABEL} />);
      await waitFor(() => expect(api.kinds()).toContain("ready"));
      const before = api.calls.filter((c) => c.kind === "resize").length;

      const term = get();
      if (term) term.nextFit = { cols: 120, rows: 40 };
      observer.fire();

      await waitFor(() =>
        expect(api.calls.at(-1)).toEqual({ kind: "resize", id: "c1", cols: 120, rows: 40 }),
      );
      // A drag fires the observer every frame; the host hears about the grid, not the frames.
      observer.fire();
      observer.fire();
      expect(api.calls.filter((c) => c.kind === "resize")).toHaveLength(before + 1);
    } finally {
      observer.restore();
    }
  });

  it("says the size was not applied, and stops saying it once one is", async () => {
    const observer = captureResizeObserver();
    try {
      const api = new FakeConsoleApi([FakeConsoleApi.running("c1")]);
      api.resizeRejects = true;
      const { factory, get } = fakeTerminalFactory({ cols: 80, rows: 24 });

      const view = render(<ConsoleView id="c1" api={api} createTerminal={factory} label={LABEL} />);

      // Non-blocking by contract: the notice appears and the grid is still there.
      expect(await view.findByTestId("console-resize-notice")).toHaveTextContent(/not applied/i);
      expect(view.getByTestId("console-grid")).toBeInTheDocument();

      api.resizeRejects = false;
      const term = get();
      if (term) term.nextFit = { cols: 100, rows: 30 };
      observer.fire();

      // One transient failure must not leave "Size not applied" up for the life of the panel.
      await waitFor(() => expect(view.queryByTestId("console-resize-notice")).toBeNull());
    } finally {
      observer.restore();
    }
  });
});

describe("a scrollback the host had to cut short", () => {
  it("says only the recent output was restored rather than passing it off as the whole history", async () => {
    const api = new FakeConsoleApi([
      FakeConsoleApi.running("c1", { providerId: "codex", sid: "a" }, 64),
    ]);
    api.scrollbackText.set("c1", "the tail end of it");
    api.scrollbackTruncated = true;
    const { factory } = fakeTerminalFactory();

    const view = render(
      <ConsoleView id="c1" api={api} createTerminal={factory} replay label={LABEL} />,
    );

    expect(await view.findByTestId("console-truncated-notice")).toHaveTextContent(/most recent/i);
    expect(view.queryByTestId("console-replay-notice")).toBeNull();
  });
});

describe("a host that cannot answer console_list at all", () => {
  it("says the console cannot be accounted for instead of inventing an exit code", async () => {
    const api = new FakeConsoleApi([FakeConsoleApi.running("c1")]);
    api.listRejects = true;
    const { factory } = fakeTerminalFactory();

    const view = render(<ConsoleView id="c1" api={api} createTerminal={factory} label={LABEL} />);

    expect(await view.findByTestId("console-gone-notice")).toBeInTheDocument();
    // An unanswerable question is not an ended process, and it is not a ready console either.
    expect(view.queryByTestId("console-exit-notice")).toBeNull();
    expect(api.kinds()).not.toContain("ready");
  });
});

describe("a keystroke the host rejects", () => {
  it("is dropped quietly rather than raising an unhandled rejection", async () => {
    const api = new FakeConsoleApi([FakeConsoleApi.running("c1")]);
    api.inputRejects = true;
    const { factory, get } = fakeTerminalFactory();

    render(<ConsoleView id="c1" api={api} createTerminal={factory} label={LABEL} />);
    await waitFor(() => expect(api.kinds()).toContain("ready"));

    get()?.type("y\r");

    await waitFor(() => expect(api.kinds()).toContain("input"));
    await new Promise((resolve) => setTimeout(resolve, 10));
  });
});
