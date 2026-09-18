# 3. Owner waits are one policy with three cases, implemented by every adapter

Date: 2026-09-18 · **Status:** accepted

## The problem

The three engines each decide "is the owner being waited on?" inside their own code, and they had
drifted into three dialects of one sentence:

- Claude publishes a status word (`needs_input`, `permission`, …) **and** a transcript tail that
  can carry a question or an interruption.
- Codex fires a `PermissionRequest` hook, but only while its rollout turn is open, and has no
  question signal at all.
- OpenCode records a pending part or a permission row, and an `MessageAbortedError` interrupt.

Nothing forced any adapter to handle all three cases, so the gaps were invisible. Two were live
bugs of the same class the product must not tell: a Codex question reads `Running`
(openai/codex#28969 — `request_user_input` has no hook event), and an OpenCode
`external_directory` approval reads `Running` (measured 2026-09-18: the `permission` table stayed
empty for the whole 8.6-minute wait, so there is no persisted trace).

## The ruling

**One `WaitPolicy` trait, implemented by each adapter, owns the three cases and their precedence.**

Rust has no inheritance; the translation of "a base class the routers inherit" is a trait with
three *required* methods and one *provided* method (`adapters/wait.rs`):

```rust
pub trait WaitPolicy {
    fn permission(&self) -> Option<WaitSignal>;
    fn question(&self) -> Option<WaitSignal>;
    fn interruption(&self) -> Option<WaitSignal>;

    fn owner_wait(&self) -> Option<WaitSignal> {
        self.permission().or_else(|| self.question()).or_else(|| self.interruption())
    }
}
```

- The three case methods are **required**: an adapter that does not answer one does not compile, so
  a case can never again be silently skipped.
- `owner_wait` is **provided**: the precedence — permission, then question, then interruption — is
  written exactly once.
- `OwnerWait::{Permission, Question} → NeedsYou`, `OwnerWait::Interruption → Waiting`.
- `None` is a stated absence ("this engine publishes no signal for this case, or none is
  outstanding"), never a failure. A source that could not be read is still an `EngineError` raised
  before the policy is built.

The live-status reads that used to live in `services/status.rs` moved to the adapters with the
policy: Claude's transcript-tail classifier (`claude_tail_facts`), Codex's rollout turn reader
(`codex::codex_turn`) and the hook mapping (`CodexWait`), and OpenCode's newest-part projection
(`OpenCodePart`) plus `OpenCodeWait`. `services/status.rs` now resolves `owner_wait()` first and
falls back to each engine's ordinary running/waiting/unknown turn call.

## Consequences

- A permission request outranks a question, and either outranks an interruption, identically for
  every engine.
- The two known gaps are pinned by tests that assert the *absence* rather than papering over it:
  `codex_has_no_question_signal_and_says_so_rather_than_inventing_one` and
  `a_permission_with_no_persisted_trace_is_a_stated_absence_not_a_guess`.
- `codex_hooks::state_for` is gone; the hook's meaning lives in `CodexWait`, where the "only over
  an open turn" rule sits beside it.
- The hook event file is now parsed as concatenated JSON (Codex sends each payload on stdin with no
  trailing newline), which is what let the Codex permission case work at all — see the parser tests
  in `codex_hooks.rs`.
- Codex questions and unpersisted OpenCode approvals remain `Running`. Closing them needs upstream
  evidence, not more code here; the policy states the gap instead of guessing.
