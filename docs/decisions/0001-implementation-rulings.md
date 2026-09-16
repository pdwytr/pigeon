# 0001 — Rulings taken while building the first pass

**Date:** 2026-09-13 · **Status:** accepted

The contracts under `docs/contracts/` specify what Pigeon does. These are the decisions the
implementation forced that the contracts did not settle, recorded here because each was a fork in
the road and the reasoning is not recoverable from the code alone.

## 1. Codex's `input_tokens` is reduced by its cached portion

**Measured:** Codex's `total_token_usage` satisfies `total = input + output` exactly, and
`cached_input_tokens` is a **subset** of `input_tokens` — 12,208,937 input containing 11,544,064
cached, on this Mac.

Claude's API reports `input_tokens` and `cache_read_input_tokens` as disjoint quantities, so
`input_tokens` means *uncached input* everywhere else in the product. Passing Codex's figure
through unchanged would do two bad things at once: double-count the cached prompt, once under
`input_tokens` and again under `cache_read`; and make a Codex row incomparable with a Claude row
in the same project card, where the two are summed into one total.

**Ruling:** subtract, saturating at zero, per `frds.md` §2.2.5. The `cached > input` guard stays
and now also protects the subtraction from underflowing.

## 2. Codex's `user_turns` falls back when the engine emits no user-message events

22 of 72 rollouts here — including the three newest — emit no `user_message` events at all, because
of Codex's paginated history mode. Counting only those events renders 0 user turns for sessions
that plainly had them.

**Ruling:** when a file yields no `user_message` events, count wrapper-filtered user
`response_item` records instead, per `frds.md` §2.2.5. A wrong number is worse than a second code
path.

## 3. An unwired or failing status source reports nothing live, never everything

`NoLiveStatus` returns an empty snapshot rather than treating every session as live.

An empty Live tab with a full Recent tab is *visibly wrong* and prompts the owner to ask why. A
Live tab full of sessions that ended days ago looks right and is a lie. The same reasoning governs
`LiveState`: a present process whose state we cannot read is `Unknown`, not an optimistic
`Running`, and a provider's status-source failure becomes an `EngineError` rather than a fake
`Unknown` session.

## 4. `closed_at` is observed where possible and falls back to the engine's own last write

Recent scope needs a close time and Pigeon keeps no store, so it cannot know when a session ended
before it started. It watches live-to-closed transitions in memory, and for everything else falls
back to `last_active_ms`.

That fallback is never *later* than the true close, so a session can be shown as closed slightly
earlier than it was but never later — the error is in the direction that does not hide a session
from the Recent window.

## 5. `MetricState` failures are cached

A transcript whose shape we cannot parse would otherwise be re-read on every pass, forever, at
whatever size it happens to be. The cache key is the source signature, so a file that is *fixed*
(by the engine writing more) gets a fresh attempt automatically.

## 6. The dev port is 1430

Demo Studio's `CLAUDE.md` reserves 1420–1422 for Aide, the owner's own `tauri dev`, and
main-tree Playwright. Vite's `strictPort` turns an overlap into a hard failure, so Pigeon stays
out of that block entirely rather than taking the scaffold's 1420.

## 7. `typecheck` runs both TypeScript projects explicitly

A bare `tsc --noEmit` type-checks neither `vite.config.ts` nor anything outside `include` — the
trap Studio's RP-4 review found on 2026-09-10. Project references were dropped because `tsc -b`
refuses a referenced project that disables emit, and both of ours are check-only.

## 8. Vitest 5, not 3

Vitest 3 pins its own Vite 7 beside the project's Vite 8. The two `Plugin` types are structurally
incompatible and the mismatch surfaces as a thirty-line overload error in `vite.config.ts`.

## 9. `dialog:allow-open`, not `dialog:default`

`dialog:default` would also grant save, message and ask — three surfaces Pigeon has no use for
and would rather not leave reachable from the WebView. The folder picker is the only dialog it
needs.

## 10. Bundle targets are `app` and `dmg`, not `all`

`all` also requests `.deb`, `.rpm` and `.msi` targets that cannot build on this machine, turning a
working macOS bundle into a failed one. Signing and notarization are out of scope for this pass.

## 11. `opencode` is spelled as one word on the wire

serde's `kebab-case` renders `OpenCode` as `open-code`; the engine's own name is one word and
`apis.md` says `opencode`. Pinned by an explicit rename and a wire-value test, which is what caught
it.

## 12. `claude agents --json` is not on the poll path

It exists and answers in ~182 ms. Reading the status file the CLI already wrote costs ~0.03 ms.
Beyond the two orders of magnitude, shelling out to an engine is a heavier commitment than reading
its output: a subprocess can prompt, can block on the network, and its argv and exit contract
belong to the engine. The file wins.

## 13. The session's configured model is never surfaced

An owner ruling from 2026-09-12, and the enforcement is structural rather than a habit: `model` is
not a field on `SessionDto` at all, so the view cannot render it even by accident. OpenCode's
`session` row and Codex's `session_meta` both state one; both stop at the adapter.

## What remains open

- **Whether a Codex resume restarts the cumulative token counter.** Unmeasurable here: zero of 68
  sessions span two files, so there is no evidence either way. The line that would need a per-file
  sum is flagged in `read_metrics`.
- **Distinguishing `Running` from `NeedsYou` for Codex and OpenCode.** Claude publishes a status
  word; the other two do not, so their live sessions are honestly `Unknown` until a better signal
  is found.
- **OpenCode attribution from the host.** Its process is visible and has a cwd, but a cwd is not
  proof of which session, so it is left unattributed rather than guessed.
