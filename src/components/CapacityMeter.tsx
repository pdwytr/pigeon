// Provider allowance, where a provider publishes one.
//
// **`supported === false` renders the words "n/a" and draws no bar at all.** A zero-width bar is
// not a neutral rendering of "we don't know" — on a meter whose full width means "all used", an
// empty track reads as "none used", which is a claim about an allowance Pigeon has never seen.
// OpenCode publishes nothing, so this is the ordinary case, not an edge one.
//
// The strip itself is not part of the dashboard mockup (components.md §4.1 says account/capacity is
// not a startup dependency and "may be added later without changing the project, session, or
// terminal component boundaries"). It is fetched after first paint and renders only when the host
// answered, so its absence changes nothing above it.

import type { AccountStatus, Capacity, ProviderId } from "../bindings";
import { PROVIDER_LABELS } from "../bindings";
import { formatPercent, formatUntil } from "../format";

const WINDOW_LABELS: Record<Capacity["windows"][number]["name"], string> = {
  five_hour: "5-hour",
  weekly: "weekly",
  monthly: "monthly",
};

/** Warm at 75%, hot at 90%. Colour is a second signal; the percentage is always spelled out. */
function tone(usedPct: number): string {
  if (usedPct >= 90) return "meter hot";
  if (usedPct >= 75) return "meter warn";
  return "meter";
}

function limitLabel(capacity: Capacity): string {
  const now = Date.now();
  const resetsAtMs = capacity.windows
    .map((window) => window.resetsAtMs)
    .filter((value): value is number => value !== null && value > now)
    .sort((a, b) => a - b)[0];
  return resetsAtMs ? `Limit reached · resets ${formatUntil(resetsAtMs)}` : "Limit reached";
}

export function CapacityMeter({ capacity }: { capacity: Capacity }) {
  if (!capacity.supported) return null;
  if (capacity.windows.length === 0) {
    return (
      <p className="account-email" data-testid={`capacity-empty-${capacity.provider}`}>
        No allowance window reported yet.
      </p>
    );
  }
  return (
    <div data-testid={`capacity-windows-${capacity.provider}`}>
      {capacity.windows.map((w) => (
        <div key={w.name}>
          {/* The bar is DECORATION. Everything it encodes is spelled out in the line under it, so
              it is hidden from assistive technology rather than dressed up in a role that would
              read the same number twice. */}
          <div
            className={tone(w.usedPct)}
            data-testid={`capacity-bar-${capacity.provider}-${w.name}`}
            aria-hidden="true"
          >
            <i style={{ width: `${Math.min(100, Math.max(0, w.usedPct))}%` }} />
          </div>
          <div className="meter-label">
            <span>
              {WINDOW_LABELS[w.name]} · {formatPercent(w.usedPct)} used
            </span>
            {/* A reset is in the FUTURE, so it counts down. Sent through a past-only formatter it
                reads "resets now" for a window three days out — which is exactly what happened. */}
            <span>{w.resetsAtMs ? `resets ${formatUntil(w.resetsAtMs)}` : "no reset stated"}</span>
          </div>
        </div>
      ))}
    </div>
  );
}

/**
 * Who this account belongs to, in one line.
 *
 * **"not signed in" is reserved for an engine that actually is not.** OpenCode states no email and
 * no plan — it identifies itself by the providers configured in its `auth.json` — so keying the
 * line off `label` alone reported a signed-in engine with two configured providers, one of them a
 * paid subscription, as signed out. `signedIn` is the field that answers this question; `label` is
 * only the nicest thing to show when there is one.
 */
function accountWho(identity: AccountStatus["identity"]): string {
  if (!identity.signedIn) return "not signed in";
  if (identity.label) return identity.label;
  const count = identity.providers?.length ?? 0;
  if (count === 0) return "signed in";
  return count === 1 ? "signed in · 1 provider" : `signed in · ${count} providers`;
}

export interface AccountsStripProps {
  accounts: Partial<Record<ProviderId, AccountStatus>>;
}

export function AccountsStrip({ accounts }: AccountsStripProps) {
  const entries = Object.values(accounts);
  if (entries.length === 0) return null;
  return (
    <section className="section" data-testid="accounts-strip">
      <h3>Provider capacity</h3>
      <div className="accounts">
        {entries.map((account) => (
          <div className="account" key={account.provider}>
            <div className="account-identity">
              <div className="account-top">
                <div className="account-name-group">
                  <span className="account-name">{PROVIDER_LABELS[account.provider]}</span>
                  {account.capacity.reachedLimit && (
                    <span className="pending" data-testid={`capacity-limit-${account.provider}`}>
                      {limitLabel(account.capacity)}
                    </span>
                  )}
                </div>
                <span className="tag">
                  {account.identity.plan ??
                    (account.capacity.supported ? "no plan stated" : "no plan")}
                </span>
              </div>
              <p className="account-email">{accountWho(account.identity)}</p>
              {account.identity.providers?.length ? (
                <div className="account-providers" data-testid={`providers-${account.provider}`}>
                  {account.identity.providers.map((p) => (
                    <span className="tag" key={`${p.name}:${p.kind}`}>
                      {p.name}
                    </span>
                  ))}
                </div>
              ) : null}
            </div>
            <div className="account-limits">
              <CapacityMeter capacity={account.capacity} />
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}
