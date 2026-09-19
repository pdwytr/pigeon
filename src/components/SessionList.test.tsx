import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { sessionKeyId } from "../bindings";
import { makeRow, T0 } from "../test/rows";
import { SessionList } from "./SessionList";

describe("a merged session list", () => {
  it("renders rows from every engine in one list ordered by activity, newest first", () => {
    const rows = [
      makeRow({ provider: "opencode", sid: "c", title: "Oldest", lastActiveMs: T0 - 3_600_000 }),
      makeRow({ provider: "claude-code", sid: "a", title: "Newest", lastActiveMs: T0 }),
      makeRow({ provider: "codex", sid: "b", title: "Middle", lastActiveMs: T0 - 600_000 }),
    ];

    render(<SessionList rows={rows} selectedKey={null} onSelect={() => {}} />);

    const rendered = screen.getAllByTestId(/^session-row-/).map((el) => el.textContent ?? "");
    expect(rendered[0]).toContain("Newest");
    expect(rendered[1]).toContain("Middle");
    expect(rendered[2]).toContain("Oldest");
  });

  it("orders a closed session by when it closed, not by when it was last touched", () => {
    const rows = [
      makeRow({ sid: "a", title: "Closed recently", lastActiveMs: T0 - 7_200_000, closedAtMs: T0 }),
      makeRow({ sid: "b", title: "Touched recently", lastActiveMs: T0 - 60_000 }),
    ];

    render(<SessionList rows={rows} selectedKey={null} onSelect={() => {}} />);

    const rendered = screen.getAllByTestId(/^session-row-/).map((el) => el.textContent ?? "");
    expect(rendered[0]).toContain("Closed recently");
  });

  it("renders both sessions when two engines mint the same sid", () => {
    // The reason `sessionKeyId` exists. A list keyed on `sid` — or on the title, which is also
    // shared here — would render one row and silently lose the other.
    const sid = "0199c4a1-2b3d-7e4f-8a9b-0c1d2e3f4a5b";
    const rows = [
      makeRow({ provider: "claude-code", sid, title: "Same name", lastActiveMs: T0 }),
      makeRow({ provider: "codex", sid, title: "Same name", lastActiveMs: T0 - 1_000 }),
    ];

    render(<SessionList rows={rows} selectedKey={null} onSelect={() => {}} />);

    expect(screen.getAllByTestId(/^session-row-/)).toHaveLength(2);
    expect(screen.getByTestId(`session-row-claude-code:${sid}`)).toBeInTheDocument();
    expect(screen.getByTestId(`session-row-codex:${sid}`)).toBeInTheDocument();
  });

  it("renders the live badge from the row's own status and an absence when there is none", () => {
    const rows = [
      makeRow({ sid: "a", title: "Working", status: "running", lastActiveMs: T0 }),
      makeRow({
        sid: "delegating",
        title: "Delegating",
        status: "delegating",
        lastActiveMs: T0 - 0.5,
      }),
      makeRow({
        provider: "codex",
        sid: "b",
        title: "Waiting",
        status: "needs_you",
        lastActiveMs: T0 - 1,
      }),
      makeRow({
        provider: "opencode",
        sid: "c",
        title: "Silent",
        status: null,
        lastActiveMs: T0 - 2,
      }),
      makeRow({
        provider: "claude-code",
        sid: "d",
        title: "Idle at prompt",
        status: "finished",
        lastActiveMs: T0 - 3,
      }),
    ];

    render(<SessionList rows={rows} selectedKey={null} onSelect={() => {}} />);

    expect(screen.getByTestId("status-running")).toHaveTextContent("running");
    expect(screen.getByTestId("status-delegating")).toHaveTextContent("delegating");
    expect(screen.getByTestId("status-needs_you")).toHaveTextContent("needs you");
    // A session the engine says nothing about is a stated absence, not "finished" and not blank —
    // and because the row carries its own aria-label, the absence has to be IN that label or a
    // screen reader would hear nothing about the status at all.
    expect(screen.getByTestId("status-absent")).toBeInTheDocument();
    expect(screen.getByTestId("status-finished")).toHaveTextContent("idle");
    expect(screen.getByTestId("session-row-opencode:c")).toHaveAccessibleName(/no status reported/);
    expect(screen.getByTestId("session-row-claude-code:a")).toHaveAccessibleName(/running/);
  });

  it("falls back to Untitled session rather than showing a fragment of the sid", () => {
    const rows = [makeRow({ sid: "0199c4c2-1188-7d55-b0e4-6a7b8c9d0e1f", title: "" })];
    render(<SessionList rows={rows} selectedKey={null} onSelect={() => {}} />);

    expect(screen.getByText("Untitled session")).toBeInTheDocument();
    expect(screen.queryByText(/0199c4c2/)).toBeNull();
  });

  it("marks the selected row with aria-current and hands the complete key back on a click", async () => {
    const onSelect = vi.fn();
    const rows = [
      makeRow({ provider: "codex", sid: "b", title: "Pick me", lastActiveMs: T0 }),
      makeRow({ provider: "claude-code", sid: "a", title: "Not me", lastActiveMs: T0 - 1 }),
    ];

    render(<SessionList rows={rows} selectedKey={rows[0].key} onSelect={onSelect} />);

    expect(screen.getByTestId(`session-row-${sessionKeyId(rows[0].key)}`)).toHaveAttribute(
      "aria-current",
      "true",
    );

    await userEvent.click(screen.getByTestId(`session-row-${sessionKeyId(rows[1].key)}`));
    expect(onSelect).toHaveBeenCalledWith({ providerId: "claude-code", sid: "a" });
  });

  it("says the list is empty rather than rendering nothing at all", () => {
    render(
      <SessionList
        rows={[]}
        selectedKey={null}
        emptyText="No live sessions."
        onSelect={() => {}}
      />,
    );
    expect(screen.getByTestId("session-list-empty")).toHaveTextContent("No live sessions.");
  });
});
