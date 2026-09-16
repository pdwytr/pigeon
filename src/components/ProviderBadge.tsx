// The engine a session belongs to, as a label.
//
// `providerId` is the API value and travels unchanged in every command and every key; what the
// badge shows is a display string derived from it. The two are kept visibly separate here so nobody
// is ever tempted to parse the label back into an id.

import { PROVIDER_LABELS, type ProviderId } from "../bindings";

/** The dense uppercase form the mockup uses. Presentation only. */
const SHORT: Record<ProviderId, string> = {
  "claude-code": "CLAUDE",
  codex: "CODEX",
  opencode: "OPENCODE",
};

export interface ProviderBadgeProps {
  provider: ProviderId;
  /** `initial` is the single letter the packed project rows use. The full name still reaches
   *  assistive technology, because one letter is not a name. */
  variant?: "short" | "initial";
}

export function ProviderBadge({ provider, variant = "short" }: ProviderBadgeProps) {
  const text = variant === "initial" ? SHORT[provider].slice(0, 1) : SHORT[provider];
  return (
    <i
      className={`engine ${provider}${variant === "initial" ? " small" : ""}`}
      data-testid={`provider-${provider}`}
      title={PROVIDER_LABELS[provider]}
    >
      {/* The abbreviation is for the eye; the engine's full name is what is announced. A generic
          element cannot carry an `aria-label` at all, so this is also the only correct way. */}
      <span aria-hidden="true">{text}</span>
      <span className="sr-only">{PROVIDER_LABELS[provider]}</span>
    </i>
  );
}
