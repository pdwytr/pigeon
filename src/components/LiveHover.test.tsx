// The always-on-top surface, and the three things it must never do: claim nothing is running while
// it is still asking, claim nothing is running when the asking failed, and lay itself out as an
// overlay over a dashboard that is not in its window.

import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import { createFakeApi, fakeApiWith } from "../api/fake";
import HoverSurface from "../HoverSurface";
import { makeRow } from "../test/rows";
import { LiveSessionRoster } from "./LiveSessionRoster";

describe("the live roster hierarchy", () => {
  it("leads with the project and moves the provider to the supporting line", () => {
    const session = makeRow({
      provider: "codex",
      project: "/work/pigeon",
      projectName: "pigeon",
      title: "Fix test suite",
      status: "finished",
    });

    render(<LiveSessionRoster sessions={[session]} />);

    const item = screen.getByRole("listitem");
    const heading = item.querySelector(".dock-session-head") as HTMLElement;
    const metadata = item.querySelector(".dock-session-meta") as HTMLElement;
    const project = within(heading).getByText("pigeon");
    expect(project.parentElement?.firstElementChild).toBe(project);
    expect(within(heading).getByText("Fix test suite")).toBeInTheDocument();
    expect(within(heading).queryByTestId("provider-codex")).toBeNull();
    expect(metadata.firstElementChild).toBe(within(metadata).getByTestId("provider-codex"));
  });
});

describe("the hover while it is still asking", () => {
  it("shows a skeleton rather than saying there are no live sessions", async () => {
    // Six agents can be running behind a hover that has simply not had its answer yet. "No live
    // sessions" is a claim about the machine; nothing has been read to support it.
    const pending = () => new Promise<never>(() => {});
    const api = fakeApiWith({ statusSnapshot: pending, sessionsList: pending });

    render(<HoverSurface api={api} pollMs={0} />);

    const hover = await screen.findByTestId("live-hover");
    expect(within(hover).getByTestId("hover-skeleton")).toBeInTheDocument();
    expect(within(hover).queryByText("No live sessions.")).toBeNull();
    // And no counter claims a zero it has not counted.
    expect(within(hover).queryByText("0")).toBeNull();
  });
});

describe("the hover when the host cannot answer", () => {
  it("says the status could not be read instead of reporting an empty machine", async () => {
    // `Promise.allSettled` drops a rejection silently, so the window said "No live sessions" with
    // the same confidence it says it when the host really has none.
    const api = fakeApiWith({
      statusSnapshot: () => Promise.reject(new Error("the host did not answer")),
    });

    render(<HoverSurface api={api} pollMs={0} />);

    const hover = await screen.findByTestId("live-hover");
    expect(await within(hover).findByTestId("hover-problem")).toHaveTextContent(
      /could not be read/i,
    );
    expect(within(hover).queryByText("No live sessions.")).toBeNull();
  });

  it("keeps the notice visible in compact mode, where the footer is not", async () => {
    // The only existing hedge — the footer's "No snapshot yet" — is `display: none` in compact
    // mode, so a notice that lived there would be invisible exactly when the window is smallest.
    const api = fakeApiWith({
      statusSnapshot: () => Promise.reject(new Error("the host did not answer")),
    });

    render(<HoverSurface api={api} pollMs={0} />);
    const hover = await screen.findByTestId("live-hover");
    const notice = await within(hover).findByTestId("hover-problem");

    expect(notice.closest("footer")).toBeNull();
  });
});

describe("the waiting-on-you offers", () => {
  beforeEach(() => window.localStorage.clear());

  it("offers once per engine, and a Yes installs that engine's bridge and says what happened", async () => {
    const api = createFakeApi();
    render(<HoverSurface api={api} pollMs={0} />);

    const codex = await screen.findByTestId("hook-prompt-codex");
    expect(within(codex).getByText(/show when Codex waits on you/i)).toBeInTheDocument();
    // Both engines are offered independently.
    expect(screen.getByTestId("hook-prompt-opencode")).toBeInTheDocument();

    fireEvent.click(within(codex).getByRole("button", { name: "Yes" }));

    await waitFor(() => expect(api.commandNames()).toContain("codex_hooks_enable"));
    expect(await screen.findByTestId("hook-prompt-message-codex")).toHaveTextContent(
      /waiting on you/i,
    );
  });

  it("installs the OpenCode bridge through its own command", async () => {
    const api = createFakeApi();
    render(<HoverSurface api={api} pollMs={0} />);

    const opencode = await screen.findByTestId("hook-prompt-opencode");
    expect(within(opencode).getByText(/show when OpenCode waits on you/i)).toBeInTheDocument();
    fireEvent.click(within(opencode).getByRole("button", { name: "Yes" }));

    await waitFor(() => expect(api.commandNames()).toContain("opencode_hooks_enable"));
    expect(await screen.findByTestId("hook-prompt-message-opencode")).toHaveTextContent(
      /Restart OpenCode/i,
    );
  });

  it("remembers Not now per engine, so dismissing one does not dismiss the other", async () => {
    const api = createFakeApi();
    const first = render(<HoverSurface api={api} pollMs={0} />);
    const codex = await screen.findByTestId("hook-prompt-codex");
    fireEvent.click(within(codex).getByRole("button", { name: "Not now" }));
    expect(screen.queryByTestId("hook-prompt-codex")).toBeNull();
    // OpenCode's offer is untouched by Codex's dismissal.
    expect(screen.getByTestId("hook-prompt-opencode")).toBeInTheDocument();
    expect(window.localStorage.getItem("pigeon.codex-hooks-dismissed")).toBe("true");

    // A fresh window on the same machine must not ask about Codex again, but still asks OpenCode.
    first.unmount();
    render(<HoverSurface api={createFakeApi()} pollMs={0} />);
    await screen.findByTestId("live-hover");
    await waitFor(() => expect(screen.queryByTestId("hook-prompt-codex")).toBeNull());
    expect(screen.getByTestId("hook-prompt-opencode")).toBeInTheDocument();
  });

  it("never offers an engine whose bridge is already installed", async () => {
    const api = createFakeApi();
    await api.codexHooksEnable();
    await api.opencodeHooksEnable();
    render(<HoverSurface api={api} pollMs={0} />);

    await screen.findByTestId("live-hover");
    await waitFor(() => expect(api.commandNames()).toContain("opencode_hooks_status"));
    expect(screen.queryByTestId("hook-prompt-codex")).toBeNull();
    expect(screen.queryByTestId("hook-prompt-opencode")).toBeNull();
  });
});

describe("the hover's own window", () => {
  it("opens provider limits from the header control", async () => {
    render(<HoverSurface api={createFakeApi()} pollMs={0} />);

    const hover = await screen.findByTestId("live-hover");
    expect(within(hover).getByRole("button", { name: "Minimize hover" })).toBeInTheDocument();
    const limits = within(hover).getByRole("button", { name: "USAGE LIMITS" });
    expect(within(hover).queryByTestId("accounts-strip")).toBeNull();

    fireEvent.click(limits);

    expect(within(hover).getByTestId("accounts-strip")).toBeInTheDocument();
    expect(limits).toHaveAttribute("aria-expanded", "true");

    const agents = within(hover).getByRole("button", { name: "← AGENTS" });
    fireEvent.click(agents);
    expect(within(hover).getByRole("button", { name: "USAGE LIMITS" })).toBeInTheDocument();
    expect(within(hover).queryByTestId("accounts-strip")).toBeNull();
  });

  it("fills it rather than floating inside it, and can be dragged by its header", async () => {
    // The stylesheet was written when the hover was an overlay over the dashboard:
    // `position: fixed; inset: 0; padding: 70px 22px 0` inside a 340×420 undecorated window leaves
    // a 70px dead band at the top, shrinks the shell and clips the footer.
    const api = createFakeApi();
    render(<HoverSurface api={api} pollMs={0} />);

    const hover = await screen.findByTestId("live-hover");
    expect(hover.parentElement).toHaveClass("window");
    // An undecorated window has no title bar; without a drag region it cannot be moved at all.
    expect(within(hover).getByTestId("hover-header")).toHaveAttribute("data-tauri-drag-region");
  });

  it("still lists the sessions the snapshot counted once both answers land", async () => {
    const api = createFakeApi();
    render(<HoverSurface api={api} pollMs={0} />);
    const hover = await screen.findByTestId("live-hover");

    const snapshot = await api.statusSnapshot();
    await waitFor(() =>
      expect(
        within(within(hover).getByRole("list", { name: "Live sessions" })).getAllByRole("listitem"),
      ).toHaveLength(snapshot.live.length),
    );
    expect(within(hover).queryByTestId("hover-problem")).toBeNull();
    expect(within(hover).queryByTestId("hover-skeleton")).toBeNull();
    expect(within(hover).getByText(/last updated/)).toBeInTheDocument();
  });
});
