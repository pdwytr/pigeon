// "+ Add session", and the three engines it can start.
//
// A menu rather than three buttons because the project card has room for one control, and the
// choice of engine is a real choice — `session_start` launches whichever installed CLI is named, in
// the project's own folder, with no resume argument.
//
// It closes on Escape and on a click outside, because a popup that only closes by choosing
// something is a trap for anyone who opened it by accident.

import { useEffect, useId, useRef, useState } from "react";
import { PROVIDER_IDS, PROVIDER_LABELS, type ProviderId } from "../bindings";

export interface EngineMenuProps {
  label?: string;
  className?: string;
  disabled?: boolean;
  onChoose(provider: ProviderId): void;
}

export function EngineMenu({
  label = "+ Add session",
  className = "",
  disabled = false,
  onChoose,
}: EngineMenuProps) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement | null>(null);
  const menuId = useId();

  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <span className="menu-wrap" ref={wrapRef}>
      <button
        type="button"
        className={className}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        disabled={disabled}
        onClick={() => setOpen((v) => !v)}
      >
        {label}
      </button>
      {open && (
        <span className="engine-menu" id={menuId} role="menu">
          {PROVIDER_IDS.map((provider) => (
            <button
              key={provider}
              type="button"
              role="menuitem"
              onClick={() => {
                setOpen(false);
                onChoose(provider);
              }}
            >
              Open in {PROVIDER_LABELS[provider]}
            </button>
          ))}
        </span>
      )}
    </span>
  );
}
