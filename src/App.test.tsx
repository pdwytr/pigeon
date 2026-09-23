import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import App from "./App";
import { createFakeApi, type FakePigeonApi, fakeApiWith, SHARED_SID } from "./api/fake";
import type { MetricState } from "./bindings";
import HoverSurface from "./HoverSurface";
import { fakeTerminalFactory } from "./test/fakeConsole";

/**
 * The whole dashboard against the in-memory host, with the terminal seam faked — xterm cannot be
 * opened under jsdom (see `console/terminalEngine.ts`), so the engine is injected here.
 *
 * Queries are scoped to a region on purpose. A session row legitimately appears twice: once in its
 * project card in the sidebar, and once in the project's session list in the detail pane. That is
 * the mockup's shape, so an unscoped `getByTestId` for a row is ambiguous by design rather than by
 * accident.
 */
function renderApp() {
  const api: FakePigeonApi = createFakeApi();
  const terminal = fakeTerminalFactory();
  render(<App api={api} terminalFactory={terminal.factory} />);
  const sidebar = () => screen.getByRole("complementary", { name: /Projects and sessions/ });
  const pane = () => screen.getByTestId("detail-pane");
  return {
    api,
    terminal,
    sidebar,
    pane,
    paneTitle: () => within(pane()).getByRole("heading", { level: 1 }),
    claudeRow: () => within(sidebar()).getByTestId(`session-row-claude-code:${SHARED_SID}`),
    codexRow: () => within(sidebar()).getByTestId(`session-row-codex:${SHARED_SID}`),
  };
}

describe("the dashboard on startup", () => {
  it("opens on the Live scope with the first project selected", async () => {
    const app = renderApp();

    expect(await screen.findByRole("tab", { name: /Live/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await waitFor(() => expect(app.paneTitle()).toHaveTextContent("demo-studio"));
    expect(screen.getByTestId("detail-eyebrow")).toHaveTextContent("Project metrics");
  });

  it("renders both sessions when two engines share one sid", async () => {
    const app = renderApp();
    // Same uuid, same project, different engines. If anything joined on sid alone, one would win.
    await waitFor(() => expect(app.claudeRow()).toBeInTheDocument());
    expect(app.codexRow()).toBeInTheDocument();
  });

  it("shows counting for a session whose fold has not landed, and never a zero", async () => {
    const app = renderApp();

    // "API adapter" is the pending row in the fixture.
    await waitFor(() => expect(within(app.sidebar()).getByText("API adapter")).toBeInTheDocument());
    await userEvent.click(
      within(app.sidebar()).getByText("API adapter").closest("button") as HTMLElement,
    );

    expect(await screen.findByTestId("metrics-pending")).toHaveTextContent(/counting/i);
  });

  it("renders the host's sentence for a session whose metrics could not be counted", async () => {
    const app = renderApp();

    await waitFor(() =>
      expect(within(app.sidebar()).getByText("Untitled session")).toBeInTheDocument(),
    );
    await userEvent.click(
      within(app.sidebar()).getByText("Untitled session").closest("button") as HTMLElement,
    );

    expect(await screen.findByTestId("metrics-unavailable")).toHaveTextContent(
      /record shape Pigeon does not recognize/,
    );
  });
});

describe("selecting a session", () => {
  it("renders its metrics in the same pane, without navigating anywhere", async () => {
    const app = renderApp();

    await waitFor(() => expect(app.claudeRow()).toBeInTheDocument());
    await userEvent.click(app.claudeRow());

    await waitFor(() => expect(app.paneTitle()).toHaveTextContent("Pigeon UI"));
    expect(screen.getByTestId("detail-eyebrow")).toHaveTextContent("Session metrics");
    // The project it belongs to is still context on the same screen.
    expect(within(app.pane()).getAllByText(/demo-studio/).length).toBeGreaterThan(0);
  });

  it("does not call console_close when the selection moves to another session", async () => {
    const app = renderApp();

    await waitFor(() => expect(app.codexRow()).toBeInTheDocument());
    // The codex session has a detached console in the fixture, so this selection attaches one.
    await userEvent.click(app.codexRow());
    expect(await screen.findByTestId("terminal-panel")).toBeInTheDocument();

    await userEvent.click(app.claudeRow());

    // Detach, not close. The host keeps the PTY and coming back is cheap.
    await waitFor(() => expect(screen.queryByTestId("terminal-panel")).toBeNull());
    expect(app.api.commandNames()).not.toContain("console_close");
  });

  it("re-adopts an existing console and replays its scrollback before its live output", async () => {
    const app = renderApp();

    await waitFor(() => expect(app.codexRow()).toBeInTheDocument());
    await userEvent.click(app.codexRow());
    await screen.findByTestId("terminal-panel");

    await waitFor(() => expect(app.api.commandNames()).toContain("console_scrollback"));
    await waitFor(() => expect(app.terminal.get()?.text ?? "").toContain("waiting for you"));
    // The frozen order: the list answer comes before the scrollback, and ready comes after both.
    const names = app.api.commandNames();
    expect(names.indexOf("console_list")).toBeLessThan(names.indexOf("console_scrollback"));
    // `indexOf`, not `lastIndexOf`: the rule is that `console_ready` comes LAST, and a
    // `lastIndexOf` comparison passes even when a ready was also sent first — which is precisely
    // the violation the frozen order forbids.
    expect(names.indexOf("console_scrollback")).toBeLessThan(names.indexOf("console_ready"));
  });

  it("explains why a session cannot be resumed instead of offering a button that would fail", async () => {
    const app = renderApp();

    await waitFor(() => expect(within(app.sidebar()).getByText("API adapter")).toBeInTheDocument());
    await userEvent.click(
      within(app.sidebar()).getByText("API adapter").closest("button") as HTMLElement,
    );

    expect(await screen.findByTestId("resume-blocked")).toHaveTextContent(
      /does not publish a resume/,
    );
    expect(screen.getByRole("button", { name: "Resume in terminal" })).toBeDisabled();
  });
});

describe("the scope switch", () => {
  it("never carries a closed session into Live, or a live one into Recent", async () => {
    const app = renderApp();

    await waitFor(() => expect(within(app.sidebar()).getByText("Pigeon UI")).toBeInTheDocument());

    await userEvent.click(screen.getByRole("tab", { name: /Recent/ }));

    // Recent holds only closed sessions, and none of them is running.
    await waitFor(() =>
      expect(within(app.sidebar()).getAllByText("Console polish").length).toBeGreaterThan(0),
    );
    expect(within(app.sidebar()).queryByText("Pigeon UI")).toBeNull();
    expect(within(app.sidebar()).queryByTestId("status-running")).toBeNull();

    await userEvent.click(screen.getByRole("tab", { name: /Live/ }));

    await waitFor(() => expect(within(app.sidebar()).getByText("Pigeon UI")).toBeInTheDocument());
    // And the closed session does not reappear with a live badge attached to it.
    expect(within(app.sidebar()).queryByText("Console polish")).toBeNull();
  });

  it("says a live session is no longer in this view rather than filing it under Recent", async () => {
    const app = renderApp();

    await waitFor(() => expect(app.claudeRow()).toBeInTheDocument());
    await userEvent.click(app.claudeRow());
    await waitFor(() => expect(app.paneTitle()).toHaveTextContent("Pigeon UI"));

    await userEvent.click(screen.getByRole("tab", { name: /Recent/ }));

    // The pane keeps rendering the session it has — blanking the numbers would be worse — but it
    // says plainly that this row is not part of what is on screen.
    expect(await screen.findByTestId("selection-stale")).toHaveTextContent(
      "No longer in this view",
    );
  });

  it("keeps one engine's failure from removing the other engines' rows", async () => {
    const app = renderApp();

    await userEvent.click(await screen.findByRole("tab", { name: /Recent/ }));

    expect(await screen.findByTestId("scope-problems")).toHaveTextContent(
      /OpenCode database was busy/,
    );
    await waitFor(() =>
      expect(within(app.sidebar()).getAllByText("Docs cleanup").length).toBeGreaterThan(0),
    );
  });
});

describe("project totals", () => {
  it("renders the host's project KPIs rather than anything derived in the View", async () => {
    const app = renderApp();

    await waitFor(() =>
      expect(within(app.pane()).getByTestId("project-kpi-contextPerCall")).toBeInTheDocument(),
    );
    expect(within(app.pane()).getByTestId("project-kpi-rewriteRatio")).toHaveAttribute(
      "data-absent",
      "false",
    );
  });

  it("says the totals are partial while some of the project's sessions are still uncounted", async () => {
    const app = renderApp();

    // demo-studio has three sessions and one of them is unavailable, so two are counted.
    expect(await within(app.pane()).findByTestId("metrics-coverage")).toHaveTextContent(
      "Counting 1 of 3 sessions",
    );
  });
});

describe("the live hover", () => {
  // The hover is a SEPARATE always-on-top window, not a panel over the dashboard. The topbar
  // button only asks the host to show that window; nothing hover-shaped may appear in this one, or
  // one click gives the owner two hovers — which is what happened until an audit found the `hover`
  // window rendering a second entire dashboard inside a 340x420 frame.
  it("asks the host to show its window and renders no hover inside the dashboard", async () => {
    const app = renderApp();

    await userEvent.click(await screen.findByRole("button", { name: /Show hover/ }));

    expect(app.api.commandNames()).toContain("hover_toggle");
    expect(screen.getByRole("button", { name: /Hide hover/ })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.queryByTestId("live-hover")).toBeNull();
  });

  it("renders the compact surface in its own window without a hidden dashboard contract", async () => {
    const api = createFakeApi();
    render(<HoverSurface api={api} pollMs={0} />);

    const hover = await screen.findByTestId("live-hover");
    // The header reads "3 open", not "3 open agent": the plate already says whose.
    const openLabel = within(hover).getByText(
      (_, el) => el?.classList.contains("dock-open-label") ?? false,
    );
    expect(openLabel).toHaveTextContent(/^\d+ open$/i);
    expect(within(hover).queryByText(/open agent/i)).toBeNull();

    expect(within(hover).queryByRole("button", { name: /Pigeon UI/ })).toBeNull();
  });

  it("lists only what the status snapshot reports as live", async () => {
    const api = createFakeApi();
    render(<HoverSurface api={api} pollMs={0} />);
    const hover = await screen.findByTestId("live-hover");

    const snapshot = await api.statusSnapshot();
    // Four rows under a headline reading three is worse than three rows, so the list and the count
    // must come from the same snapshot. The rows are the hover's own buttons; the two control
    // buttons in its header are not rows.
    const list = within(hover).getByRole("list", { name: "Live sessions" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(snapshot.live.length);
    expect(snapshot.live.length).toBeGreaterThan(0);
  });

  it("stops polling when it is unmounted, rather than leaving an interval running", async () => {
    // The risk is a LEAKED interval, which a pollMs of 0 cannot produce and an unmount in the same
    // tick cannot catch. So: a real interval, proven to have fired, then proven to have stopped.
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const api = createFakeApi();
      const { unmount } = render(<HoverSurface api={api} pollMs={5000} />);
      await screen.findByTestId("live-hover");
      const atStart = api.commandNames().length;

      await act(async () => {
        await vi.advanceTimersByTimeAsync(11_000);
      });
      const whilePolling = api.commandNames().length;
      expect(whilePolling).toBeGreaterThan(atStart);

      unmount();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(30_000);
      });

      expect(api.commandNames().length).toBe(whilePolling);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("adding a session to a project", () => {
  it("renders the terminal it just started, with a way to close it", async () => {
    // `session_start` spawns a real PTY and returns a console whose `sessionKey` is null — there is
    // no session for it until the engine writes a discoverable record. The pane rendered a terminal
    // only in its SESSION branch, and only when the console's key matched the selected session, so
    // an engine started here ran in a terminal nobody could see, with no Close button, until
    // Pigeon exited.
    const app = renderApp();
    await waitFor(() => expect(app.paneTitle()).toHaveTextContent("demo-studio"));

    await userEvent.click(within(app.pane()).getByRole("button", { name: "+ Add session" }));
    await userEvent.click(within(app.pane()).getByRole("menuitem", { name: /Claude Code/ }));

    const panel = await within(app.pane()).findByTestId("terminal-panel");
    expect(app.api.commandNames()).toContain("session_start");
    expect(within(panel).getByRole("button", { name: /Close terminal/ })).toBeInTheDocument();
    expect(within(panel).getByText(/new session/)).toBeInTheDocument();
  });

  it("closes that terminal when the owner asks, and stops rendering it", async () => {
    const app = renderApp();
    await waitFor(() => expect(app.paneTitle()).toHaveTextContent("demo-studio"));

    await userEvent.click(within(app.pane()).getByRole("button", { name: "+ Add session" }));
    await userEvent.click(within(app.pane()).getByRole("menuitem", { name: /Codex/ }));
    const panel = await within(app.pane()).findByTestId("terminal-panel");

    await userEvent.click(within(panel).getByRole("button", { name: /Close terminal/ }));

    await waitFor(() => expect(screen.queryByTestId("terminal-panel")).toBeNull());
    expect(app.api.commandNames()).toContain("console_close");
  });

  it("finds that terminal again after the owner looks at a session and comes back", async () => {
    // Detaching is not closing: the PTY outlives the selection that opened it. A project console
    // the pane could not re-adopt would be the same invisible-process bug in another shape.
    const app = renderApp();
    await waitFor(() => expect(app.paneTitle()).toHaveTextContent("demo-studio"));

    await userEvent.click(within(app.pane()).getByRole("button", { name: "+ Add session" }));
    await userEvent.click(within(app.pane()).getByRole("menuitem", { name: /Codex/ }));
    await within(app.pane()).findByTestId("terminal-panel");

    await userEvent.click(app.claudeRow());
    await waitFor(() => expect(app.paneTitle()).toHaveTextContent("Pigeon UI"));
    expect(screen.queryByTestId("terminal-panel")).toBeNull();
    expect(app.api.commandNames()).not.toContain("console_close");

    await userEvent.click(
      within(app.sidebar()).getByText("demo-studio").closest("button") as HTMLElement,
    );

    expect(await within(app.pane()).findByTestId("terminal-panel")).toBeInTheDocument();
  });

  it("shows it even when the owner was looking at a session when they asked", async () => {
    // The menu is on every project card in the sidebar too, and a console with no session cannot
    // be shown in the session branch of the pane. Asking for an engine in a project is a request
    // to look at that project.
    const app = renderApp();
    await waitFor(() => expect(app.claudeRow()).toBeInTheDocument());
    await userEvent.click(app.claudeRow());
    await waitFor(() => expect(app.paneTitle()).toHaveTextContent("Pigeon UI"));

    const card = within(app.sidebar()).getByTestId(
      "project-card-/Users/khalid/Documents/Projects/demo-studio",
    );
    await userEvent.click(within(card).getByRole("button", { name: "+ Add session" }));
    await userEvent.click(within(card).getByRole("menuitem", { name: /OpenCode/ }));

    await waitFor(() => expect(app.paneTitle()).toHaveTextContent("demo-studio"));
    expect(await within(app.pane()).findByTestId("terminal-panel")).toBeInTheDocument();
  });
});

describe("a metric fold that lands after its row was listed", () => {
  it("replaces counting… on the row itself, not only in the metrics section", async () => {
    // The row rendered `row.metrics` — a copy frozen into the last `sessions_list` answer — while
    // `sessions://metrics` was merged into the store. One session, one screen, two numbers.
    const app = renderApp();
    await waitFor(() => expect(within(app.sidebar()).getByText("pigeon")).toBeInTheDocument());
    await userEvent.click(
      within(app.sidebar()).getByText("pigeon").closest("button") as HTMLElement,
    );

    const pending = await within(app.pane()).findByText(/counting…/);
    expect(pending).toBeInTheDocument();

    const landed: MetricState = {
      state: "ready",
      value: {
        inputTokens: 5_400,
        outputTokens: 7_100,
        cacheRead: 504_000,
        cacheWrite: 20_160,
        apiCalls: 12,
        toolCalls: 19,
        userTurns: 4,
        durationMs: 1_320_000,
        reasoningTokens: null,
        providerCostUsd: null,
        kpis: { contextPerCall: 42_000, rewriteRatio: 0.04, batchingRatio: 1.58 },
      },
    };
    act(() =>
      app.api.emitMetrics([
        {
          key: { providerId: "opencode", sid: "0199c4b8-77aa-7c31-9f20-5d6e7f809a1b" },
          metrics: landed,
        },
      ]),
    );

    await waitFor(() => expect(within(app.pane()).queryByText(/counting…/)).toBeNull());
    expect(within(app.pane()).getByText("12 calls")).toBeInTheDocument();
  });
});

describe("a projects_summary the host could not answer", () => {
  it("says there was a problem instead of claiming there are no live projects", async () => {
    const api = fakeApiWith({
      projectsSummary: () => Promise.reject(new Error("The project index could not be read.")),
    });
    render(<App api={api} terminalFactory={fakeTerminalFactory().factory} />);

    const sidebar = screen.getByRole("complementary", { name: /Projects and sessions/ });
    expect(await within(sidebar).findByTestId("project-list-problem")).toHaveTextContent(
      /could not be read/,
    );
    expect(within(sidebar).queryByTestId("project-list-empty")).toBeNull();
  });
});
