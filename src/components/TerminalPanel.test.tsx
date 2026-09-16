// The panel around a console — and the one thing it owns that the console itself cannot: which
// console this is. `ConsoleView` keeps `phase`, `exitCode`, `truncated`, `replaying` and the
// resize notice in local state, so the panel has to tell React that a different console id is a
// different component instance.

import { render, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { ConsoleSummary } from "../bindings";
import type { TerminalFactory } from "../console/terminalEngine";
import { FakeConsoleApi, fakeTerminalFactory, gatedTerminalFactory } from "../test/fakeConsole";
import { TerminalPanel } from "./TerminalPanel";

const LABEL = "Codex terminal for Fix test suite in demo-studio";

function panel(summary: ConsoleSummary, api: FakeConsoleApi, factory: TerminalFactory) {
  return (
    <TerminalPanel
      summary={summary}
      api={api}
      terminalFactory={factory}
      hostOs={null}
      replay={summary.scrollbackBytes > 0}
      label={LABEL}
      onClose={() => {}}
    />
  );
}

const SECOND: ConsoleSummary = { ...FakeConsoleApi.running("c2"), id: "c2" };

describe("switching the panel from one console to another", () => {
  it("does not leave the first console's exit notice above the second one's terminal", async () => {
    const first = FakeConsoleApi.running("c1");
    const api = new FakeConsoleApi([first, SECOND]);
    const opening = fakeTerminalFactory();

    const view = render(panel(first, api, opening.factory));
    await waitFor(() => expect(api.kinds()).toContain("ready"));
    api.emitExit("c1", 2);
    expect(await view.findByTestId("console-exit-notice")).toHaveTextContent("exit code 2");

    // The second console's attach is held open, which is what the owner sees for real: the fonts
    // alone take up to 1.5 s. With one reused instance, "Process ended — exit code 2" sits there
    // the whole time, over a console that has not ended.
    const gated = gatedTerminalFactory();
    view.rerender(panel(SECOND, api, gated.factory));

    expect(view.queryByTestId("console-exit-notice")).toBeNull();
    expect(view.getByTestId("console-view")).toHaveAttribute("data-phase", "attaching");

    gated.release();
    await waitFor(() =>
      expect(view.getByTestId("console-view")).toHaveAttribute("data-phase", "live"),
    );
  });

  it("does not carry the first console's partial-history notice onto the second", async () => {
    // `truncated` is written by the scrollback restore and nothing ever clears it, so a reused
    // instance tells the owner the NEXT console's history was cut short.
    const withHistory = FakeConsoleApi.running("c1", { providerId: "codex", sid: "a" }, 64);
    const api = new FakeConsoleApi([withHistory, SECOND]);
    api.scrollbackTruncated = true;
    api.scrollbackText.set("c1", "only the last of it");
    const { factory } = fakeTerminalFactory();

    const view = render(panel(withHistory, api, factory));
    expect(await view.findByTestId("console-truncated-notice")).toBeInTheDocument();

    view.rerender(panel(SECOND, api, factory));

    await waitFor(() =>
      expect(view.getByTestId("console-view")).toHaveAttribute("data-phase", "live"),
    );
    expect(view.queryByTestId("console-truncated-notice")).toBeNull();
  });
});
