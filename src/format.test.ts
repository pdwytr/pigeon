import { describe, expect, it } from "vitest";
import {
  ABSENT,
  formatAgoShort,
  formatDuration,
  formatNullable,
  formatRatio,
  formatTokens,
  formatUntil,
  pathLeaf,
  shortenPath,
  totalTokens,
} from "./format";

describe("token counts", () => {
  it("uses the compact form the project cards and metric cards share", () => {
    expect(formatTokens(15_100_000)).toBe("15.1M");
    expect(formatTokens(420_000)).toBe("420K");
    expect(formatTokens(512)).toBe("512");
  });

  it("renders a real zero as 0, because a session that spent nothing is a measurement", () => {
    expect(formatTokens(0)).toBe("0");
  });

  it("drops a trailing .0 so 1,000,000 is 1M rather than 1.0M", () => {
    expect(formatTokens(1_000_000)).toBe("1M");
  });
});

describe("the token total every surface shares", () => {
  it("is input + output + cache read, and counts a cache write in neither", () => {
    // Three surfaces wrote this sum out by hand — the project card, the project rail and the
    // session row — which is three chances for one screen to disagree with itself.
    expect(totalTokens({ inputTokens: 1_000, outputTokens: 2_000, cacheRead: 400_000 })).toBe(
      403_000,
    );
    // A cache WRITE is the cost of putting the context there; adding it to the reads of that same
    // context counts it twice.
    expect(
      totalTokens({ inputTokens: 0, outputTokens: 0, cacheRead: 0, cacheWrite: 36_000 } as {
        inputTokens: number;
        outputTokens: number;
        cacheRead: number;
      }),
    ).toBe(0);
  });
});

describe("nullable figures", () => {
  it("renders an em dash for null and never a zero", () => {
    expect(formatNullable(null, formatRatio)).toBe(ABSENT);
    expect(formatNullable(null, formatRatio)).not.toBe("0.00");
  });

  it("renders a measured zero as 0.00", () => {
    expect(formatNullable(0, formatRatio)).toBe("0.00");
  });
});

describe("relative time", () => {
  const now = 1_760_000_000_000;

  it("says now inside the first minute", () => {
    expect(formatAgoShort(now - 5_000, now)).toBe("now");
  });

  it("counts minutes, then hours, then names yesterday", () => {
    expect(formatAgoShort(now - 4 * 60_000, now)).toBe("4m");
    expect(formatAgoShort(now - 2 * 3_600_000, now)).toBe("2h");
    expect(formatAgoShort(now - 30 * 3_600_000, now)).toBe("yesterday");
  });

  it("falls back to a date once a week has passed", () => {
    // Not a bare "40d": beyond the recent window a row's age stops being the useful fact and the
    // day it happened starts being one.
    //
    // The assertion this replaces was `toMatch(/\d/)`, which "40d" satisfies — it matched the one
    // rendering the rule exists to rule out.
    const then = now - 40 * 24 * 3_600_000;
    const out = formatAgoShort(then, now);

    expect(out).not.toMatch(/^\d+[mhd]$/);
    expect(out).not.toBe("yesterday");
    expect(out).toBe(
      new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(then),
    );
  });
});

describe("durations", () => {
  it("renders hours, minutes and seconds without a date library", () => {
    expect(formatDuration(45_000)).toBe("45s");
    expect(formatDuration(4 * 60_000 + 12_000)).toBe("4m 12s");
    expect(formatDuration(3_600_000 + 4 * 60_000)).toBe("1h 04m");
    // A three-day allowance window read "71h 59m" before this tier existed.
    expect(formatDuration(3 * 24 * 3_600_000 - 60_000)).toBe("2d 23h");
  });
});

describe("paths", () => {
  it("abbreviates a home directory without losing the folder that identifies the project", () => {
    const full = "/Users/khalid/Documents/Projects/demo-studio";
    const shortened = shortenPath(full);

    expect(shortened).toBe("~/Documents/Projects/demo-studio");
    // A visual truncation, so: the same input shortens the same way every time, the leaf the
    // owner recognises the project by survives it, and the home directory it stood for is gone.
    //
    // (What this replaces was `expect(full).toBe("<that same literal>")` — a local constant
    // asserted against its own definition, which is true of every string in every program.)
    expect(shortenPath(shortened)).toBe(shortened);
    expect(pathLeaf(shortened)).toBe(pathLeaf(full));
    expect(shortened).not.toContain("/Users/khalid");
  });

  it("elides the middle of a long path rather than cutting off the folder that matters", () => {
    const shortened = shortenPath("/Users/khalid/a/b/c/d/e/pigeon");
    expect(shortened).toContain("pigeon");
    expect(shortened).toContain("…");
  });

  it("handles a Windows path the same way", () => {
    expect(shortenPath("C:\\Users\\khalid\\Projects\\pigeon")).toBe("~\\Projects\\pigeon");
  });

  it("takes the leaf folder regardless of separator", () => {
    expect(pathLeaf("/Users/khalid/Projects/pigeon")).toBe("pigeon");
    expect(pathLeaf("C:\\wt\\lane1\\demo-studio")).toBe("demo-studio");
  });
});

describe("a point in the future", () => {
  const now = 1_760_000_000_000;

  it("counts down rather than saying now, which is what a past-only formatter would say", () => {
    // The bug this was written for: `formatAgoShort` on a future timestamp gives a negative delta,
    // lands in its under-a-minute branch, and renders an allowance resetting in three days as "now".
    expect(formatUntil(now + 96 * 60_000, now)).toBe("in 1h 36m");
    expect(formatAgoShort(now + 96 * 60_000, now)).toBe("now");
  });

  it("says a window is due rather than counting backwards past zero", () => {
    expect(formatUntil(now - 1_000, now)).toBe("due now");
  });
});
