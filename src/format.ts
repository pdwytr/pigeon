// Presentation only. Everything here turns a number Rust already computed into characters.
//
// Two rules this file exists to keep in one place:
//
//   * **A null is a stated absence, not a zero.** `formatNullable` returns the em dash and its
//     callers render a dimmed chip; nothing here ever substitutes 0 for a missing measurement.
//   * **Timestamps are UTC epoch milliseconds until the last possible moment.** They are formatted
//     with `Intl`, never with a date library, and never stored back in local form.

const EM_DASH = "—";

/** The one spelling of "no measurement", so a test can assert it and a grep can find it. */
export const ABSENT = EM_DASH;

// ------------------------------------------------------------------------------------- numbers

/** Token counts, in the mockup's compact form: `15.1M`, `420K`, `512`. A real zero is `0`. */
export function formatTokens(n: number): string {
  const abs = Math.abs(n);
  if (abs >= 1_000_000) return `${trimZero(n / 1_000_000)}M`;
  if (abs >= 1_000) return `${trimZero(n / 1_000)}K`;
  return String(Math.round(n));
}

function trimZero(v: number): string {
  const s = v.toFixed(1);
  return s.endsWith(".0") ? s.slice(0, -2) : s;
}

/**
 * The tokens a session or a project spent, as every surface counts them: input + output + cache
 * read. One exported formula because it was written out three times — the project card, the
 * project rail and the session row — and three copies of an arithmetic rule are three chances for
 * the same screen to disagree with itself.
 *
 * `cacheWrite` is deliberately NOT in it: a cache write is the cost of PUTTING something in the
 * cache, and adding it to the reads of that same content would count the same context twice.
 */
export function totalTokens(m: {
  inputTokens: number;
  outputTokens: number;
  cacheRead: number;
}): number {
  return m.inputTokens + m.outputTokens + m.cacheRead;
}

/** Plain counts — API calls, tool calls, turns. Grouped, because six digits without separators is
 *  a number nobody reads correctly at a glance. */
export function formatCount(n: number): string {
  return new Intl.NumberFormat(undefined).format(n);
}

/** A ratio KPI (`rewriteRatio`, `batchingRatio`). Two decimals, so `0` reads as `0.00` and is
 *  visibly a measurement rather than a blank. */
export function formatRatio(n: number): string {
  return n.toFixed(2);
}

/** `null` → the em dash. The caller is expected to ALSO mark the chip absent for assistive tech;
 *  a dash alone is a visual signal and colour/shape is never the only carrier of a state. */
export function formatNullable(n: number | null, fmt: (v: number) => string): string {
  return n === null ? ABSENT : fmt(n);
}

/** The engine's own cost figure, when it states one. Pigeon never invents a price, so a `null`
 *  here means the chip is omitted entirely rather than shown as `$0.00`. */
export function formatUsd(n: number): string {
  return new Intl.NumberFormat(undefined, { style: "currency", currency: "USD" }).format(n);
}

export function formatPercent(pct: number): string {
  return `${Math.round(pct)}%`;
}

/** A span of milliseconds as `3d 00h` / `1h 04m` / `4m 12s`. Durations only, never a clock time.
 *
 *  The day tier is not decoration: a weekly allowance window resets three days out, and `71h 59m`
 *  is a number nobody converts in their head. */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  const d = Math.floor(total / 86_400);
  const h = Math.floor((total % 86_400) / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (d > 0) return `${d}d ${String(h).padStart(2, "0")}h`;
  if (h > 0) return `${h}h ${String(m).padStart(2, "0")}m`;
  if (m > 0) return `${m}m ${String(s).padStart(2, "0")}s`;
  return `${s}s`;
}

// --------------------------------------------------------------------------------------- time

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** The dense form the mockup's rows use: `now`, `4m`, `2h`, `yesterday`, then a date.
 *
 *  Deliberately not `Intl.RelativeTimeFormat`: at `narrow` style it still renders "4m ago", and
 *  these rows are 10px columns where the suffix is both noise and a wrap risk. The prose form
 *  below is the one that carries the units, and it is what the row's `title`/label uses. */
export function formatAgoShort(atMs: number, nowMs: number = Date.now()): string {
  const delta = nowMs - atMs;
  if (delta < MINUTE) return "now";
  if (delta < HOUR) return `${Math.floor(delta / MINUTE)}m`;
  if (delta < DAY) return `${Math.floor(delta / HOUR)}h`;
  if (delta < 2 * DAY) return "yesterday";
  if (delta < 7 * DAY) return `${Math.floor(delta / DAY)}d`;
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(atMs);
}

/** The prose form: `4 minutes ago`, `yesterday`. This is what assistive technology reads. */
export function formatAgo(atMs: number, nowMs: number = Date.now()): string {
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  const delta = nowMs - atMs;
  if (delta < MINUTE) return "just now";
  if (delta < HOUR) return rtf.format(-Math.floor(delta / MINUTE), "minute");
  if (delta < DAY) return rtf.format(-Math.floor(delta / HOUR), "hour");
  if (delta < 30 * DAY) return rtf.format(-Math.floor(delta / DAY), "day");
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium" }).format(atMs);
}

/** A point in the FUTURE, as a countdown: `in 1h 36m`. `formatAgoShort` cannot do this — a future
 *  timestamp gives it a negative delta, which falls through its first branch and renders as "now",
 *  so an allowance window resetting in three days would have said it resets now. */
export function formatUntil(atMs: number, nowMs: number = Date.now()): string {
  const delta = atMs - nowMs;
  if (delta <= 0) return "due now";
  return `in ${formatDuration(delta)}`;
}

/** A wall clock at the display edge — the topbar's `Updated 10:42:18`. */
export function formatClock(atMs: number): string {
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(atMs);
}

// --------------------------------------------------------------------------------------- paths

/** A path shortened for display only. The full `cwd` always stays available as a `title` and as
 *  assistive text — this is a visual truncation, never a new value. */
export function shortenPath(cwd: string, maxSegments = 4): string {
  const home = /^(?:\/(?:Users|home)\/[^/]+|[A-Za-z]:\\Users\\[^\\]+)/.exec(cwd);
  let out = home ? `~${cwd.slice(home[0].length)}` : cwd;
  const sep = out.includes("\\") ? "\\" : "/";
  const parts = out.split(sep).filter(Boolean);
  if (parts.length > maxSegments) {
    out = `${out.startsWith("~") ? "~" : ""}${sep}…${sep}${parts.slice(-maxSegments + 1).join(sep)}`;
  }
  return out;
}

/** The leaf folder — what a project card leads with when it has no better name. */
export function pathLeaf(cwd: string): string {
  const parts = cwd.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? cwd;
}
