// A merged, ordered list of sessions from every engine.
//
// "Merged" is the interesting word. The host returns one array already containing Claude Code,
// Codex and OpenCode rows; this component sorts them into one activity order rather than grouping
// by engine, because the owner's question is "what moved last", not "what did Codex do".
//
// The key is `sessionKeyId(row.key)`. See `SessionListRow` for why that matters and what breaks
// when it is a sid.

import type { ConsoleSummary, MetricState, SessionKey, SessionRow } from "../bindings";
import { sessionKeyId } from "../bindings";
import { sortSessions } from "../store/selectors";
import { type RowVariant, SessionListRow } from "./SessionListRow";

export interface SessionListProps {
  rows: SessionRow[];
  /** The store's metric map, keyed by `sessionKeyId`. Rows carry their own copy from the list
   *  answer they arrived in; this is the live one. */
  metrics?: Record<string, MetricState>;
  selectedKey: SessionKey | null;
  /** Consoles the host currently holds, so a row can say "terminal" without asking. */
  consoles?: Record<string, ConsoleSummary>;
  variant?: RowVariant;
  showProject?: boolean;
  emptyText?: string;
  label?: string;
  onSelect(key: SessionKey): void;
}

function consoleIdsBySession(consoles: Record<string, ConsoleSummary>): Set<string> {
  const out = new Set<string>();
  for (const summary of Object.values(consoles)) {
    if (summary.sessionKey && summary.state !== "closed") out.add(sessionKeyId(summary.sessionKey));
  }
  return out;
}

export function SessionList({
  rows,
  metrics,
  selectedKey,
  consoles = {},
  variant = "flat",
  showProject,
  emptyText = "No sessions.",
  label,
  onSelect,
}: SessionListProps) {
  const ordered = sortSessions(rows);
  const attached = consoleIdsBySession(consoles);
  const selectedId = selectedKey ? sessionKeyId(selectedKey) : null;

  if (ordered.length === 0) {
    return (
      <p className="empty-live" data-testid="session-list-empty">
        {emptyText}
      </p>
    );
  }

  return (
    // A real list, because that is what it is: a screen reader announces how many sessions there
    // are and where in them the cursor is, which a stack of sibling buttons cannot say.
    <ul
      className={variant === "project" ? "project-session-list" : "session-stack"}
      data-testid="session-list"
      aria-label={label}
    >
      {ordered.map((row) => {
        const id = sessionKeyId(row.key);
        return (
          <li key={id}>
            <SessionListRow
              row={row}
              metrics={metrics?.[id]}
              selected={id === selectedId}
              consoleAttached={attached.has(id)}
              variant={variant}
              showProject={showProject}
              onSelect={onSelect}
            />
          </li>
        );
      })}
    </ul>
  );
}
