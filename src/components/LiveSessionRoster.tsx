import type { SessionRow } from "../bindings";
import { sessionKeyId } from "../bindings";
import { formatAgoShort } from "../format";
import { ProviderBadge } from "./ProviderBadge";
import { sessionLabel } from "./SessionListRow";
import { StatusBadge } from "./StatusBadge";

interface LiveSessionRosterProps {
  sessions: SessionRow[];
}

/** The only repeated content inside the dock: one readable, non-interactive live session row. */
export function LiveSessionRoster({ sessions }: LiveSessionRosterProps) {
  return (
    <ul className="dock-roster" aria-label="Live sessions">
      {sessions.map((session) => (
        <li key={sessionKeyId(session.key)} className="dock-roster-row">
          <div className="dock-session">
            <div className="dock-session-head">
              <span className="dock-session-name">
                <span className="dock-project-name">
                  {session.repositoryName ?? folderName(session.cwd, session.projectName)}
                </span>{" "}
                {sessionLabel(session)}
              </span>
              <StatusBadge status={session.status} dot={false} />
            </div>
            <div className="dock-session-meta" title={session.cwd ?? session.projectName}>
              <ProviderBadge provider={session.key.providerId} />
              {session.status === "running" ? null : <> · {recencyLabel(session.lastActiveMs)}</>}
            </div>
          </div>
        </li>
      ))}
    </ul>
  );
}

function recencyLabel(atMs: number): string {
  const value = formatAgoShort(atMs);
  if (value === "now") return "last updated just now";
  if (/^[0-9]+[mhd]$/.test(value)) return `last updated ${value} ago`;
  return `last updated ${value}`;
}

function folderName(cwd: string | null, fallback: string): string {
  const parts = cwd?.split(/[\\/]/).filter(Boolean);
  return parts?.at(-1) || fallback;
}
