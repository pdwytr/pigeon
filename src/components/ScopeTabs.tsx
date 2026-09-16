// Live / Recent, as a real tablist.
//
// A real one, not two styled buttons: `role="tablist"` with roving focus, arrow-key movement and
// `aria-selected`, because this control is how the whole dashboard changes meaning and a keyboard
// user has to be able to reach it. The panel it controls is the sidebar's project stack, which
// carries the matching `role="tabpanel"` and `aria-labelledby`.
//
// Changing scope does not mutate source data and does not clear the selection — it re-queries. The
// detail pane decides what to say if the selected session is not in the new scope.

import type { Scope } from "../bindings";

const TABS: { scope: Scope; label: string; hint: string }[] = [
  { scope: "live", label: "Live", hint: "active projects + sessions" },
  { scope: "recent", label: "Recent", hint: "projects and sessions · 7 days" },
];

export const tabId = (scope: Scope) => `scope-tab-${scope}`;
export const panelId = (scope: Scope) => `scope-panel-${scope}`;

export interface ScopeTabsProps {
  value: Scope;
  loading: boolean;
  onChange(scope: Scope): void;
}

export function ScopeTabs({ value, loading, onChange }: ScopeTabsProps) {
  const move = (delta: number) => {
    const index = TABS.findIndex((t) => t.scope === value);
    const next = TABS[(index + delta + TABS.length) % TABS.length];
    onChange(next.scope);
    // Follow the selection with focus, which is what "automatic activation" tabs do.
    document.getElementById(tabId(next.scope))?.focus();
  };

  return (
    <div className="view-switch" role="tablist" aria-label="Session scope">
      {TABS.map((tab) => {
        const selected = tab.scope === value;
        return (
          <button
            key={tab.scope}
            type="button"
            id={tabId(tab.scope)}
            role="tab"
            aria-selected={selected}
            aria-controls={panelId(tab.scope)}
            aria-busy={selected && loading}
            tabIndex={selected ? 0 : -1}
            onClick={() => onChange(tab.scope)}
            onKeyDown={(e) => {
              if (e.key === "ArrowRight" || e.key === "ArrowDown") {
                e.preventDefault();
                move(1);
              } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
                e.preventDefault();
                move(-1);
              }
            }}
          >
            {tab.label}
            <small>{tab.hint}</small>
          </button>
        );
      })}
    </div>
  );
}
