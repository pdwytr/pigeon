// A session's status, in words and in colour — never in colour alone (WCAG 1.4.1).
//
// `null` is not a status. It is the host saying it has no observation, and it renders as a stated
// absence with a name assistive technology can read, rather than as a blank cell that looks like a
// rendering bug or, worse, as "finished".

import type { SessionStatus } from "../bindings";

const WORDS: Record<SessionStatus, string> = {
  running: "running",
  delegating: "delegating",
  needs_you: "needs you",
  finished: "idle",
  unknown: "unknown",
};

/** The status in words, absence included. Exported because a row that carries its own `aria-label`
 *  REPLACES its children in the accessibility tree — so the badge's text would be silently dropped
 *  unless the row's own label says the same thing. One function, so the two cannot drift. */
export function statusWording(status: SessionStatus | null): string {
  return status === null ? "no status reported" : WORDS[status];
}

export interface StatusBadgeProps {
  status: SessionStatus | null;
  /** Drop the leading dot in the tightest rows. */
  dot?: boolean;
}

export function StatusBadge({ status, dot = true }: StatusBadgeProps) {
  if (status === null) {
    return (
      <span className="status absent" data-testid="status-absent">
        <span aria-hidden="true">—</span>
        <span className="sr-only">{statusWording(null)}</span>
      </span>
    );
  }
  return (
    <span className={`status ${status}`} data-testid={`status-${status}`}>
      {dot ? <span aria-hidden="true">● </span> : null}
      {WORDS[status]}
    </span>
  );
}
