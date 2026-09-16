import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { AccountStatus, Capacity } from "../bindings";

import { AccountsStrip, CapacityMeter } from "./CapacityMeter";

function capacity(over: Partial<Capacity>): Capacity {
  return {
    provider: "opencode",
    supported: false,
    windows: [],
    plan: null,
    stale: false,
    sourceAgeS: null,
    reachedLimit: null,
    readAtMs: 1_760_000_000_000,
    problem: null,
    ...over,
  };
}

describe("a provider that publishes no allowance", () => {
  it("renders no allowance copy or bar", () => {
    render(<CapacityMeter capacity={capacity({ supported: false, provider: "opencode" })} />);

    expect(screen.queryByText(/allowance n\/a/i)).toBeNull();
    // A zero-width bar on a meter whose full width means "all used" reads as "none used", which is
    // a claim about an allowance Pigeon has never seen.
    expect(screen.queryByTestId(/^capacity-bar-/)).toBeNull();
  });

  it("renders the same concise limit message for every provider", () => {
    for (const provider of ["claude-code", "codex", "opencode"] as const) {
      render(
        <AccountsStrip
          accounts={{
            [provider]: {
              provider,
              identity: {
                provider,
                signedIn: true,
                label: null,
                organization: null,
                plan: null,
                tier: null,
                mode: null,
                accountShort: null,
                providers: null,
                readAtMs: 1_760_000_000_000,
                problem: null,
              },
              capacity: capacity({
                provider,
                reachedLimit: "provider-specific-limit-token",
              }),
            },
          }}
        />,
      );

      expect(screen.getByTestId(`capacity-limit-${provider}`)).toHaveTextContent("Limit reached");
      expect(screen.getByTestId(`capacity-limit-${provider}`).parentElement).toHaveClass(
        "account-name-group",
      );
      expect(screen.queryByText("provider-specific-limit-token")).toBeNull();
    }
  });

  it("shows the earliest available reset beside a reached limit", () => {
    render(
      <AccountsStrip
        accounts={{
          "claude-code": {
            provider: "claude-code",
            identity: {
              provider: "claude-code",
              signedIn: true,
              label: "Claude",
              organization: null,
              plan: null,
              tier: null,
              mode: null,
              accountShort: null,
              providers: null,
              readAtMs: 1_760_000_000_000,
              problem: null,
            },
            capacity: capacity({
              provider: "claude-code",
              reachedLimit: "provider-specific-limit-token",
              supported: true,
              windows: [
                {
                  name: "weekly",
                  windowMinutes: 10_080,
                  usedPct: 100,
                  resetsAtMs: Date.now() + 96 * 60_000,
                },
              ],
            }),
          },
        }}
      />,
    );

    expect(screen.getByTestId("capacity-limit-claude-code")).toHaveTextContent(
      /Limit reached · resets in 1h 3[56]m/,
    );
  });
});

describe("a provider that does publish one", () => {
  it("draws a bar per window and spells the percentage out beside it", () => {
    render(
      <CapacityMeter
        capacity={capacity({
          provider: "claude-code",
          supported: true,
          windows: [
            { name: "five_hour", windowMinutes: 300, usedPct: 42, resetsAtMs: null },
            { name: "weekly", windowMinutes: 10_080, usedPct: 68, resetsAtMs: null },
          ],
        })}
      />,
    );

    expect(screen.getAllByTestId(/^capacity-bar-claude-code/)).toHaveLength(2);
    // Colour and width are second signals; the number is always readable as text.
    expect(screen.getByText(/5-hour · 42% used/)).toBeInTheDocument();
    expect(screen.getByText(/weekly · 68% used/)).toBeInTheDocument();
  });

  it("counts down to the reset rather than reporting a future time as though it had passed", () => {
    const now = Date.now();
    render(
      <CapacityMeter
        capacity={capacity({
          provider: "claude-code",
          supported: true,
          windows: [
            { name: "five_hour", windowMinutes: 300, usedPct: 42, resetsAtMs: now + 96 * 60_000 },
          ],
        })}
      />,
    );

    expect(screen.getByText(/resets in 1h 3[56]m/)).toBeInTheDocument();
  });

  it("says an allowance window has not been reported rather than drawing an empty track", () => {
    render(
      <CapacityMeter capacity={capacity({ provider: "codex", supported: true, windows: [] })} />,
    );

    expect(screen.getByTestId("capacity-empty-codex")).toBeInTheDocument();
    expect(screen.queryByTestId(/^capacity-bar-/)).toBeNull();
  });
});

describe("an engine that identifies itself by its providers", () => {
  /** OpenCode states no email and no plan; its `auth.json` names the providers instead. */
  const opencode: AccountStatus = {
    provider: "opencode",
    identity: {
      provider: "opencode",
      signedIn: true,
      label: null,
      organization: null,
      plan: null,
      tier: null,
      mode: null,
      accountShort: null,
      providers: [
        { name: "opencode", kind: "api" },
        { name: "opencode-go", kind: "api" },
      ],
      readAtMs: 1_760_000_000_000,
      problem: null,
    },
    capacity: capacity({ supported: false, provider: "opencode" }),
  };

  it("names each configured provider instead of dropping them", () => {
    // The host returns both of these from auth.json; the strip rendered only `label`, so a paid
    // subscription the owner had configured never appeared anywhere in the app.
    render(<AccountsStrip accounts={{ opencode }} />);

    const providers = screen.getByTestId("providers-opencode");
    expect(providers).toHaveTextContent("opencode");
    expect(providers).toHaveTextContent("opencode-go");
  });

  it("does not report a signed-in engine as signed out", () => {
    render(<AccountsStrip accounts={{ opencode }} />);

    // `signedIn` answers this question. `label` is only the nicest thing to show when there is one.
    expect(screen.queryByText(/not signed in/i)).toBeNull();
    expect(screen.getByText(/signed in/i)).toBeInTheDocument();
  });

  it("still says so when an engine really is signed out", () => {
    const out: AccountStatus = {
      ...opencode,
      identity: { ...opencode.identity, signedIn: false, providers: null },
    };
    render(<AccountsStrip accounts={{ opencode: out }} />);

    expect(screen.getByText(/not signed in/i)).toBeInTheDocument();
  });

  it("does not let a stale identity label hide a signed-out state", () => {
    const out: AccountStatus = {
      ...opencode,
      identity: {
        ...opencode.identity,
        signedIn: false,
        label: "stale-account@example.com",
        providers: null,
      },
    };
    render(<AccountsStrip accounts={{ opencode: out }} />);

    expect(screen.getByText("not signed in")).toBeInTheDocument();
    expect(screen.queryByText("stale-account@example.com")).toBeNull();
  });
});
