// The top strip: what Pigeon is, when it last heard from the host, and two controls.
//
// `lastUpdatedMs` is a UTC epoch millisecond until it reaches `formatClock` on the line below —
// formatting at the display edge, and only there, is what keeps every stored and derived value
// comparable regardless of where the window happens to be.

import { formatClock } from "../format";

export interface TopbarProps {
  lastUpdatedMs: number | null;
  refreshing: boolean;
  hoverVisible: boolean;
  onRefresh(): void;
  onToggleHover(): void;
}

export function Topbar({
  lastUpdatedMs,
  refreshing,
  hoverVisible,
  onRefresh,
  onToggleHover,
}: TopbarProps) {
  return (
    <header className="topbar">
      <div className="brand">
        PIGEON
        <small>agent session monitor</small>
      </div>
      <div className="top-actions">
        <span className="subtle" data-testid="last-updated">
          {lastUpdatedMs === null ? "Not yet updated" : `Updated ${formatClock(lastUpdatedMs)}`}
        </span>
        <button
          type="button"
          className="icon-btn"
          aria-label="Refresh data"
          // Disabled only while a refresh is being STARTED; repeated refreshes are otherwise safe,
          // and a button that stays dead until every response lands feels broken.
          disabled={refreshing}
          onClick={onRefresh}
        >
          <span className={refreshing ? "spin" : undefined} aria-hidden="true">
            ↻
          </span>
          <span className="sr-only">{refreshing ? "Refreshing" : "Refresh"}</span>
        </button>
        <button
          type="button"
          className="pill-btn"
          aria-pressed={hoverVisible}
          onClick={onToggleHover}
        >
          {hoverVisible ? "Hide hover" : "Show hover"}
        </button>
      </div>
    </header>
  );
}
