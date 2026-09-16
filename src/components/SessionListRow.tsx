// One session, as a row. Three densities, one component.
//
// **The row's React key is `sessionKeyId(row.key)` and nothing else.** Not the title (two sessions
// are called "Untitled session" in the fixture alone), not the sid (two engines legitimately mint
// the same uuid — the fixture has a pair), not the array index (which changes every time the list
// re-sorts, which is every refresh). This is the single rule the whole identity design exists for,
// and `SessionList` below is where it is applied.
//
// The row is a real `<button>`, not a div with an onClick: keyboard users select with Enter/Space,
// focus is visible, and the selected row exposes `aria-current` rather than only a background tint.

import type { MetricState, SessionKey, SessionRow } from "../bindings";
import { sessionKeyId } from "../bindings";
import { formatAgo, formatAgoShort, formatCount, formatTokens, totalTokens } from "../format";
import { ProviderBadge } from "./ProviderBadge";
import { StatusBadge, statusWording } from "./StatusBadge";

/** A session with no title of its own. Never a truncated sid — a sid fragment looks like an
 *  identity and is not one. */
const UNTITLED = "Untitled session";

export function sessionLabel(row: SessionRow): string {
  const named = (row.name ?? row.title ?? "").trim();
  return named.length > 0 ? named : UNTITLED;
}

export type RowVariant = "project" | "flat" | "detail";

export interface SessionListRowProps {
  row: SessionRow;
  /** The store's current metric state for this session. `row.metrics` is a COPY frozen into the
   *  last `sessions_list` answer, so a fold that landed since — every `sessions://metrics` event —
   *  never reaches it, and the row goes on saying "counting…" beside a metrics pane full of
   *  numbers. The store's map is the one place a session's numbers live. */
  metrics?: MetricState;
  selected: boolean;
  /** Whether the host holds a console for this session. Shown as a quiet marker, not as a status. */
  consoleAttached?: boolean;
  variant?: RowVariant;
  /** Show the project name on the row. The flat "all sessions" list needs it; a project's own
   *  grouped rows do not. */
  showProject?: boolean;
  onSelect(key: SessionKey): void;
}

export function SessionListRow({
  row,
  metrics,
  selected,
  consoleAttached = false,
  variant = "flat",
  showProject = variant === "flat",
  onSelect,
}: SessionListRowProps) {
  const label = sessionLabel(row);
  const when = row.closedAtMs ?? row.lastActiveMs;
  const className =
    variant === "project" ? "project-session" : variant === "detail" ? "session" : "session-small";
  // The full identity, the full time and the full path reach assistive technology even where the
  // visible row has room for none of them.
  const described = `${label}, ${statusWording(row.status)}, ${row.projectName}, ${formatAgo(when)}${
    consoleAttached ? ", terminal open" : ""
  }`;

  const state = metrics ?? row.metrics;

  if (variant === "detail") {
    const m = state.state === "ready" ? state.value : null;
    return (
      <button
        type="button"
        className={className}
        aria-current={selected ? "true" : undefined}
        aria-label={described}
        data-testid={`session-row-${sessionKeyId(row.key)}`}
        onClick={() => onSelect(row.key)}
      >
        <span className="session-head">
          <span className="session-name">
            <ProviderBadge provider={row.key.providerId} /> {label}
          </span>
          <StatusBadge status={row.status} />
        </span>
        <span className="session-meta">
          {m ? (
            <>
              <span>{formatCount(m.apiCalls)} calls</span>
              <span aria-hidden="true">·</span>
              <span>{formatCount(m.toolCalls)} tools</span>
              <span aria-hidden="true">·</span>
              <span>{formatTokens(totalTokens(m))} tokens</span>
              <span aria-hidden="true">·</span>
            </>
          ) : state.state === "pending" ? (
            <>
              <span className="pending">counting…</span>
              <span aria-hidden="true">·</span>
            </>
          ) : state.state === "unavailable" ? (
            <>
              <span className="pending">metrics unavailable</span>
              <span aria-hidden="true">·</span>
            </>
          ) : null}
          <span>{formatAgoShort(when)}</span>
          {consoleAttached && <span className="tag">terminal</span>}
        </span>
      </button>
    );
  }

  return (
    <button
      type="button"
      className={className}
      aria-current={selected ? "true" : undefined}
      aria-label={described}
      data-testid={`session-row-${sessionKeyId(row.key)}`}
      onClick={() => onSelect(row.key)}
    >
      <ProviderBadge
        provider={row.key.providerId}
        variant={variant === "project" ? "initial" : "short"}
      />
      <span className={variant === "project" ? "session-label" : "session-name"}>{label}</span>
      {showProject && <span className="session-project">{row.projectName}</span>}
      <StatusBadge status={row.status} dot={variant === "project"} />
      <span className={variant === "project" ? "session-age" : "session-time"}>
        {formatAgoShort(when)}
      </span>
    </button>
  );
}
