// The IPC seam, pinned.
//
// Tauri v2 resolves each command parameter by looking up its own name — lower-camel-cased from the
// Rust identifier — at the TOP LEVEL of the invoke payload. This file shipped wrapping every
// payload in `{ args: {...} }`, which hides every parameter; an audit found that every command
// taking a required argument was being rejected with "missing required key", while the six taking
// none, plus `account_status` (whose two parameters are both `Option` and so tolerate a missing
// key), worked — which is exactly why the app looked alive.
//
// The table below is transcribed from the `#[tauri::command]` signatures in
// `src-tauri/src/api/commands.rs`. It is the thing that has to stay true.

import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn(async () => ({}));

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

const { createTauriApi } = await import("./client");

/** command -> the exact top-level keys its Rust signature requires or accepts. */
const SIGNATURES: Record<string, string[]> = {
  host_info: [],
  settings_get: [],
  settings_set: ["patch"],
  sessions_list: ["scope", "force"],
  projects_summary: ["scope"],
  session_metrics: ["key"],
  status_snapshot: [],
  account_status: ["force", "provider"],
  project_pick: [],
  folder_open: ["cwd"],
  session_start: ["provider", "cwd", "cols", "rows"],
  session_stop: ["key"],
  console_open: ["sessionKey", "cwd", "cols", "rows"],
  console_ready: ["id"],
  console_input: ["id", "dataB64"],
  console_resize: ["id", "cols", "rows"],
  console_close: ["id"],
  console_list: [],
  console_scrollback: ["id", "maxBytes"],
  hover_toggle: [],
  hover_select: ["key"],
  codex_hooks_status: [],
  codex_hooks_enable: [],
  codex_hooks_disable: [],
  opencode_hooks_status: [],
  opencode_hooks_enable: [],
  opencode_hooks_disable: [],
};

/** command -> the keys its Rust signature REQUIRES. The table above says which keys are allowed;
 *  this one says which may not be missing, and without it a command that stopped sending `scope`
 *  would stay green — every key it sent would still be an allowed one. */
const REQUIRED: Record<string, string[]> = {
  host_info: [],
  settings_get: [],
  settings_set: ["patch"],
  sessions_list: ["scope"],
  projects_summary: ["scope"],
  session_metrics: ["key"],
  status_snapshot: [],
  // Both parameters are `Option`, which is why this command kept working through the envelope bug.
  account_status: [],
  project_pick: [],
  folder_open: ["cwd"],
  session_start: ["provider", "cwd", "cols", "rows"],
  session_stop: ["key"],
  console_open: ["sessionKey", "cwd", "cols", "rows"],
  console_ready: ["id"],
  console_input: ["id", "dataB64"],
  console_resize: ["id", "cols", "rows"],
  console_close: ["id"],
  console_list: [],
  console_scrollback: ["id"],
  hover_toggle: [],
  hover_select: ["key"],
  codex_hooks_status: [],
  codex_hooks_enable: [],
  codex_hooks_disable: [],
  opencode_hooks_status: [],
  opencode_hooks_enable: [],
  opencode_hooks_disable: [],
};

const KEY = { providerId: "claude-code" as const, sid: "0199-a-complete-uuid" };

/** One call per command, with arguments shaped the way the app really sends them. */
async function callEveryCommand() {
  const api = createTauriApi();
  await api.hostInfo();
  await api.settingsGet();
  await api.settingsSet({ recentWindowDays: 14 });
  await api.sessionsList({ scope: "live", force: true });
  await api.projectsSummary({ scope: "recent" });
  await api.sessionMetrics({ key: KEY });
  await api.statusSnapshot();
  await api.accountStatus({ force: true });
  await api.projectPick();
  await api.folderOpen({ cwd: "/tmp" });
  await api.sessionStart({ provider: "codex", cwd: "/tmp", cols: 80, rows: 24 });
  await api.sessionStop({ key: KEY });
  await api.consoleOpen({ sessionKey: KEY, cwd: "/tmp", cols: 80, rows: 24 });
  await api.consoleReady({ id: "c1" });
  await api.consoleInput({ id: "c1", dataB64: "aGk=" });
  await api.consoleResize({ id: "c1", cols: 100, rows: 40 });
  await api.consoleClose({ id: "c1" });
  await api.consoleList();
  await api.consoleScrollback({ id: "c1", maxBytes: 1024 });
  await api.hoverToggle();
  await api.hoverSelect(KEY);
  await api.codexHooksStatus();
  await api.codexHooksEnable();
  await api.codexHooksDisable();
  await api.opencodeHooksStatus();
  await api.opencodeHooksEnable();
  await api.opencodeHooksDisable();
}

describe("the invoke payload", () => {
  beforeEach(() => invoke.mockClear());

  it("never wraps arguments in an envelope", async () => {
    await callEveryCommand();
    for (const [command, args] of invoke.mock.calls as unknown as [string, unknown][]) {
      expect(Object.keys((args ?? {}) as object), `${command} sent an envelope`).not.toContain(
        "args",
      );
    }
  });

  it("sends only keys the Rust signature names, at the top level", async () => {
    await callEveryCommand();
    const seen = new Set<string>();
    for (const [command, args] of invoke.mock.calls as unknown as [string, unknown][]) {
      seen.add(command);
      const allowed = SIGNATURES[command];
      expect(allowed, `${command} is not in the signature table`).toBeDefined();
      for (const key of Object.keys((args ?? {}) as object)) {
        expect(allowed, `${command} sent an unknown key "${key}"`).toContain(key);
      }
    }
    // Every command in the table is exercised, so a new one cannot be added untested.
    expect([...seen].sort()).toEqual(Object.keys(SIGNATURES).sort());
  });

  it("sends every key the Rust signature requires", async () => {
    await callEveryCommand();
    for (const [command, args] of invoke.mock.calls as unknown as [string, unknown][]) {
      const required = REQUIRED[command];
      expect(required, `${command} is not in the required-key table`).toBeDefined();
      const sent = Object.keys((args ?? {}) as object);
      for (const key of required) {
        expect(sent, `${command} left out the required key "${key}"`).toContain(key);
      }
    }
  });

  it("puts a session key under its parameter name, not spread across the payload", async () => {
    await callEveryCommand();
    const metrics = (invoke.mock.calls as unknown as [string, Record<string, unknown>][]).find(
      ([c]) => c === "session_metrics",
    );
    expect(metrics?.[1]).toEqual({ key: KEY });

    const select = (invoke.mock.calls as unknown as [string, Record<string, unknown>][]).find(
      ([c]) => c === "hover_select",
    );
    // `hover_select(key: SessionKey)` takes the key as a NAMED parameter. Spreading the key's own
    // fields to the top level would send `providerId`/`sid` and miss `key` entirely.
    expect(select?.[1]).toEqual({ key: KEY });
  });

  it("sends the complete sid, never a prefix", async () => {
    await callEveryCommand();
    const open = (invoke.mock.calls as unknown as [string, Record<string, unknown>][]).find(
      ([c]) => c === "console_open",
    );
    expect(JSON.stringify(open?.[1])).toContain(KEY.sid);
  });
});
