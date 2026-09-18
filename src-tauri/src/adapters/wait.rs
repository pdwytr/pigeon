//! The one place an engine is allowed to say "the owner is being waited on".
//!
//! Every engine reaches that state differently — Claude publishes a status word plus a transcript
//! tail, Codex fires a hook but only while its rollout turn is open, OpenCode leaves a pending
//! part in its event stream. Left to themselves those become three dialects of the same sentence,
//! and they had already drifted: a Codex question reads `Running`, an OpenCode `external_directory`
//! approval reads `Running`, and nothing forced either adapter to admit the gap.
//!
//! **Rust has no inheritance, so this is the translation of "a base class the routers inherit."**
//! The three case methods are *required*: an adapter that does not answer one does not compile,
//! so a case can never be silently forgotten again. [`WaitPolicy::owner_wait`] is *provided*: the
//! precedence is written once, here, instead of once per engine.
//!
//! The facts come from the caller because only the status service holds the process table and the
//! per-engine files it already read for turn state; the policy is the pure decision over them.

use crate::domain::LiveState;

/// The three ways an engine is parked on the owner, in one vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerWait {
    /// The engine needs an approval before it can continue. The owner is being waited on.
    Permission,
    /// The engine asked a question and cannot proceed without an answer. Same, in the owner's
    /// words.
    Question,
    /// The owner (or the engine) cut the turn short; the engine is back at its prompt.
    Interruption,
}

impl OwnerWait {
    /// What this interaction shows as. Permission and question leave the owner in control; an
    /// interruption gives the turn back, which is ordinary waiting.
    pub fn state(self) -> LiveState {
        match self {
            OwnerWait::Permission | OwnerWait::Question => LiveState::NeedsYou,
            OwnerWait::Interruption => LiveState::Waiting,
        }
    }

    /// The word carried into `raw_word` when the engine has no better one of its own.
    pub fn word(self) -> &'static str {
        match self {
            OwnerWait::Permission => "permission",
            OwnerWait::Question => "question",
            OwnerWait::Interruption => "interruption",
        }
    }
}

/// One engine's proof of an owner interaction, in the shared shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitSignal {
    pub case: OwnerWait,
    /// The engine's own word for it (`PermissionRequest`, `needs_input`, …), quoted rather than
    /// translated away. `None` lets the caller keep whatever word it already had.
    pub raw_word: Option<String>,
    /// One sentence naming the evidence, never a command line.
    pub evidence: Option<String>,
}

impl WaitSignal {
    pub fn new(case: OwnerWait) -> Self {
        Self {
            case,
            raw_word: None,
            evidence: None,
        }
    }

    pub fn word(mut self, word: impl Into<String>) -> Self {
        self.raw_word = Some(word.into());
        self
    }

    pub fn because(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = Some(evidence.into());
        self
    }
}

/// The three cases every engine must answer, and the one precedence they all inherit.
///
/// `None` means "this engine publishes no signal for this case, or none is outstanding" — it is
/// a stated absence, never a failure. Callers that could not read a source must return the error
/// before building a policy, so there is no `Result` here to be mistaken for "we could not look."
pub trait WaitPolicy {
    fn permission(&self) -> Option<WaitSignal>;
    fn question(&self) -> Option<WaitSignal>;
    fn interruption(&self) -> Option<WaitSignal>;

    /// The order is the ruling: a permission request outranks a question, and either outranks an
    /// interruption. Engines that can observe more than one at once resolve it identically here.
    fn owner_wait(&self) -> Option<WaitSignal> {
        self.permission()
            .or_else(|| self.question())
            .or_else(|| self.interruption())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An engine that reports exactly the cases a test asks for, so the precedence can be pinned
    /// without dragging in three file formats.
    #[derive(Default)]
    struct Cases {
        permission: bool,
        question: bool,
        interruption: bool,
    }

    impl WaitPolicy for Cases {
        fn permission(&self) -> Option<WaitSignal> {
            self.permission
                .then(|| WaitSignal::new(OwnerWait::Permission))
        }
        fn question(&self) -> Option<WaitSignal> {
            self.question.then(|| WaitSignal::new(OwnerWait::Question))
        }
        fn interruption(&self) -> Option<WaitSignal> {
            self.interruption
                .then(|| WaitSignal::new(OwnerWait::Interruption))
        }
    }

    #[test]
    fn permission_outranks_a_question_which_outranks_an_interruption() {
        let all = Cases {
            permission: true,
            question: true,
            interruption: true,
        };
        assert_eq!(all.owner_wait().unwrap().case, OwnerWait::Permission);

        let q_and_i = Cases {
            permission: false,
            question: true,
            interruption: true,
        };
        assert_eq!(q_and_i.owner_wait().unwrap().case, OwnerWait::Question);

        let i_only = Cases {
            permission: false,
            question: false,
            interruption: true,
        };
        assert_eq!(i_only.owner_wait().unwrap().case, OwnerWait::Interruption);
    }

    #[test]
    fn no_case_is_a_stated_absence_not_a_failure() {
        assert!(Cases::default().owner_wait().is_none());
    }

    #[test]
    fn permission_and_question_need_the_owner_and_an_interruption_hands_the_turn_back() {
        assert_eq!(OwnerWait::Permission.state(), LiveState::NeedsYou);
        assert_eq!(OwnerWait::Question.state(), LiveState::NeedsYou);
        assert_eq!(OwnerWait::Interruption.state(), LiveState::Waiting);
    }
}
