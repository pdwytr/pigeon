// The wiring, driven as a hook.
//
// These are the rules that live between the reducer and the api — the ones no component test can
// reach, because they are about WHEN a request is issued rather than what is rendered when it
// lands. Each one below was a bug first.

import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { createFakeApi, fakeApiWith, SHARED_SID } from "../api/fake";
import type { Settings } from "../bindings";
import { usePigeonApp } from "./usePigeonApp";

/** Let every queued promise and effect settle, inside `act`, without owning a timer. */
async function settle(ms = 40) {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, ms));
  });
}

describe("a scoped fetch the host refuses", () => {
  it("is asked once and never retried in a loop", async () => {
    // The failure this exists for: `sessions/failed` clears `loading` and leaves `generatedAtMs`
    // null, so the follow-up effect's condition was satisfied again by the very state the
    // rejection produced — fail, refetch, fail, forever, a request token per cycle.
    let sessionCalls = 0;
    let projectCalls = 0;
    const api = fakeApiWith({
      sessionsList: () => {
        sessionCalls += 1;
        return Promise.reject(new Error("the host did not answer"));
      },
      projectsSummary: () => {
        projectCalls += 1;
        return Promise.reject(new Error("the host did not answer"));
      },
    });

    const { result } = renderHook(() => usePigeonApp(api, 0));
    await waitFor(() => expect(result.current.state.sessions.error.live).not.toBeNull());

    const first = sessionCalls;
    await settle();

    expect(sessionCalls).toBe(first);
    expect(sessionCalls).toBe(1);
    expect(projectCalls).toBe(1);
  });

  it("still refetches that scope when the owner asks for a refresh", async () => {
    let fail = true;
    const api = fakeApiWith({
      sessionsList(args: { scope: "live" | "recent"; force?: boolean }) {
        if (fail) return Promise.reject(new Error("the host did not answer"));
        return Object.getPrototypeOf(this).sessionsList.call(this, args);
      },
    });

    const { result } = renderHook(() => usePigeonApp(api, 0));
    await waitFor(() => expect(result.current.state.sessions.error.live).not.toBeNull());

    fail = false;
    act(() => result.current.actions.refresh());
    await waitFor(() => expect(result.current.state.sessions.live.length).toBeGreaterThan(0));
    expect(result.current.state.sessions.error.live).toBeNull();
  });
});

describe("startup", () => {
  it("asks for each scope's rows and projects exactly once", async () => {
    // The follow-up effect closes over the render BEFORE the startup dispatch lands, so it saw
    // `loading: false` and asked for everything a second time.
    const api = createFakeApi();
    renderHook(() => usePigeonApp(api, 0));
    await waitFor(() => expect(api.commandNames()).toContain("sessions_list"));
    await settle();

    expect(api.commandNames().filter((c) => c === "sessions_list")).toHaveLength(1);
    expect(api.commandNames().filter((c) => c === "projects_summary")).toHaveLength(1);
  });
});

describe("a console this session opened", () => {
  it("replays its scrollback when the pane comes back to it", async () => {
    // "Fresh" is true for the FIRST attach only. Written on open and deleted only on close, it
    // stayed true forever: leaving a console and returning to it replayed nothing and the owner
    // was handed an empty grid where 200 lines had been.
    const api = createFakeApi();
    const { result } = renderHook(() => usePigeonApp(api, 0));
    await waitFor(() => expect(result.current.state.sessions.live.length).toBeGreaterThan(0));

    const rows = result.current.state.sessions.live;
    const claude = rows.find((r) => r.key.providerId === "claude-code" && r.key.sid === SHARED_SID);
    const codex = rows.find((r) => r.key.providerId === "codex" && r.key.sid === SHARED_SID);
    if (!claude || !codex) throw new Error("the fixture lost its two same-sid sessions");

    act(() => result.current.actions.selectSession(claude.key));
    act(() => result.current.actions.resume(claude));
    await waitFor(() => expect(result.current.state.visibleConsoleId).not.toBeNull());
    const opened = result.current.state.visibleConsoleId;
    // Nothing to replay on the attach that opened it.
    expect(result.current.replayVisible).toBe(false);

    act(() => result.current.actions.selectSession(codex.key));
    await waitFor(() => expect(result.current.state.visibleConsoleId).not.toBe(opened));

    act(() => result.current.actions.selectSession(claude.key));
    await waitFor(() => expect(result.current.state.visibleConsoleId).toBe(opened));
    expect(result.current.replayVisible).toBe(true);
  });
});

describe("the hover toggle", () => {
  it("re-reads the host's real visibility when the dashboard regains focus", async () => {
    // The hover's own × closes its window and tells nobody. There is no visibility event in the
    // wire contract, so the dashboard re-reads `settings_get` on focus; without it the button
    // stayed inverted and "Hide hover" made the hover appear.
    const api = createFakeApi();
    const { result } = renderHook(() => usePigeonApp(api, 0));
    await waitFor(() => expect(result.current.state.settings).not.toBeNull());

    act(() => result.current.actions.toggleHover());
    await waitFor(() => expect(result.current.state.hoverVisible).toBe(true));

    // The hover closes itself, exactly as its × does.
    await act(async () => {
      await api.hoverToggle();
    });
    expect(result.current.state.hoverVisible).toBe(true);

    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });
    await waitFor(() => expect(result.current.state.hoverVisible).toBe(false));
  });
});

describe("the tab the owner left open", () => {
  it("is the one the app opens on, and is not re-applied afterwards", async () => {
    // `settings.list.view` was fetched and never read, so the store always booted to Live whatever
    // the owner had been working in.
    const settings: Settings = {
      hover: { visible: false, corner: "tr", x: null, y: null },
      list: { view: "recent" },
      pollIntervalSeconds: 5,
      recentWindowDays: 7,
      split: 0.34,
    };
    const api = fakeApiWith({ settingsGet: () => Promise.resolve(settings) });

    const { result } = renderHook(() => usePigeonApp(api, 0));
    await waitFor(() => expect(result.current.state.scope).toBe("recent"));
    await waitFor(() => expect(result.current.state.sessions.recent.length).toBeGreaterThan(0));

    // Adopted once. The focus re-read below exists for the hover's visibility, and must not drag
    // the tab back under an owner who has just changed it.
    act(() => result.current.actions.setScope("live"));
    await act(async () => {
      window.dispatchEvent(new Event("focus"));
    });
    await settle(20);

    expect(result.current.state.scope).toBe("live");
  });
});
