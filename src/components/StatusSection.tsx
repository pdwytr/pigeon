// What the host can prove about a session right now.
//
// The evidence is the point. "running" on its own is an assertion; "running, pid 4214, matched by
// cwd" is something the owner can check. The engine's own word is quoted back rather than
// translated away, for the same reason: `rawWord` is what Codex or Claude Code actually said, and
// Pigeon's `needs_you` is an interpretation of it.
//
// A Recent session shows `finished` or nothing at all and never a live observation; the observation
// is only ever passed in for the Live scope (see `selectors.liveStateFor`).

import type { LiveSessionState, SessionRow } from "../bindings";
import { formatAgo, formatDuration } from "../format";
import { StatusBadge } from "./StatusBadge";

export interface StatusSectionProps {
  row: SessionRow;
  observation: LiveSessionState | null;
}

export function StatusSection({ row, observation }: StatusSectionProps) {
  const when = row.closedAtMs ?? row.lastActiveMs;
  const inState = observation?.sinceMs ? formatDuration(Date.now() - observation.sinceMs) : null;

  return (
    <section className="section" data-testid="status-section">
      <div className="row spread">
        <h3>Session status</h3>
        <StatusBadge status={row.status} />
      </div>
      <p className="subtle">
        {row.projectName}
        {row.gitBranch ? ` · ${row.gitBranch}` : ""} · last activity {formatAgo(when)}
        {inState ? ` · ${inState} in state` : ""}
      </p>
      {observation && (
        <p className="subtle" data-testid="status-evidence">
          {observation.rawWord ? `The engine says "${observation.rawWord}". ` : ""}
          {observation.evidence.join(" · ")}
        </p>
      )}
      {row.sourceSummary && <p className="subtle">{row.sourceSummary}</p>}
      {Object.keys(row.diagnostics.unknownTypes).length > 0 && (
        <p className="pending" data-testid="status-diagnostics">
          Unrecognized record shapes:{" "}
          {Object.entries(row.diagnostics.unknownTypes)
            .map(([kind, count]) => `${kind} × ${count}`)
            .join(", ")}
        </p>
      )}
    </section>
  );
}
