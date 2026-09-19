//! Live status: an observation about a process, not a permanent fact about a session.

use serde::{Deserialize, Serialize};

use super::SessionKey;

/// Whether an engine process for this session is open anywhere on the host right now.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessPresence {
    Present,
    Absent,
}

/// What a live process is doing. `Finished` is deliberately absent: a finished session has no
/// live process, so it is a *session* display state, never a live observation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveState {
    /// The engine is working.
    Running,
    /// The parent is waiting while one or more delegated child agents are still working.
    Delegating,
    /// The engine is idle at its prompt after completing a turn.
    Waiting,
    /// The engine is waiting on the owner — a prompt, a permission, a choice.
    NeedsYou,
    /// A process is present but the engine publishes nothing we can read. Honest, not a guess.
    Unknown,
}

/// What a row shows. The union of a live observation and "this one is closed".
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Delegating,
    NeedsYou,
    Finished,
    Unknown,
}

impl From<LiveState> for SessionStatus {
    fn from(value: LiveState) -> Self {
        match value {
            LiveState::Running => SessionStatus::Running,
            LiveState::Delegating => SessionStatus::Delegating,
            LiveState::Waiting => SessionStatus::Finished,
            LiveState::NeedsYou => SessionStatus::NeedsYou,
            LiveState::Unknown => SessionStatus::Unknown,
        }
    }
}

/// One current observation per session key. Exists only while a process is open somewhere.
#[derive(Clone, Debug, PartialEq)]
pub struct LiveObservation {
    pub key: SessionKey,
    pub process: ProcessPresence,
    pub state: LiveState,
    pub since_ms: Option<i64>,
    /// The engine's own word (`busy`, `idle`, `needs_input`), quoted back rather than translated
    /// away. The owner can see what the engine actually said.
    pub raw_word: Option<String>,
    /// Bounded sentences naming *why* we believe this. Never a command line, never a path we
    /// were not already willing to show.
    pub evidence: Vec<String>,
    pub pid: Option<u32>,
    pub console_id: Option<String>,
    /// Number of child-agent transcripts whose final state is not complete.
    pub active_subagents: u32,
    pub observed_at_ms: i64,
}

/// One session's line in a [`StatusSnapshot::signature`]: who it is, what it is doing, the word
/// the engine used, the process and console behind it, and when that began.
///
/// A struct rather than a tuple because it is compared, sorted and read by people; and it carries
/// **no timestamp of its own**, which is the entire point — see [`StatusSnapshot::signature`].
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LiveSignature {
    pub session: String,
    pub state: LiveState,
    pub raw_word: Option<String>,
    pub pid: Option<u32>,
    pub console_id: Option<String>,
    pub since_ms: Option<i64>,
    pub active_subagents: u32,
}

/// The canonical projection the View renders. `live` holds only sessions with a live process.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatusSnapshot {
    pub generated_at_ms: i64,
    pub live: Vec<LiveObservation>,
}

impl StatusSnapshot {
    /// What this snapshot *says*, with the clock left out.
    ///
    /// Every observation carries `observed_at_ms`, and the snapshot carries `generated_at_ms`, so
    /// two polls of a completely unchanged machine are never `==`. A caller that compares whole
    /// snapshots to decide whether to notify therefore notifies every single poll — which is what
    /// the background worker did until an audit ran two polls back to back and diffed them.
    ///
    /// This is the comparison that answers "did anything actually change": the sessions, their
    /// states, the engine's own word for each, the owning pid and console, and when each began.
    /// Sorted, because the order observations arrive in is an artefact of which probe answered
    /// first and is not a change in the world.
    pub fn signature(&self) -> Vec<LiveSignature> {
        let mut out: Vec<LiveSignature> = self
            .live
            .iter()
            .map(|o| LiveSignature {
                session: o.key.id(),
                state: o.state,
                raw_word: o.raw_word.clone(),
                pid: o.pid,
                console_id: o.console_id.clone(),
                since_ms: o.since_ms,
                active_subagents: o.active_subagents,
            })
            .collect();
        out.sort();
        out
    }

    /// Counts over live entries only. A closed session contributes to nothing here.
    pub fn counts(&self) -> (u32, u32, u32) {
        let mut running = 0;
        let mut needs_you = 0;
        let mut unknown = 0;
        for obs in &self.live {
            match obs.state {
                LiveState::Running | LiveState::Delegating => running += 1,
                LiveState::Waiting => {}
                LiveState::NeedsYou => needs_you += 1,
                LiveState::Unknown => unknown += 1,
            }
        }
        (running, needs_you, unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ProviderId;

    fn obs(state: LiveState) -> LiveObservation {
        LiveObservation {
            key: SessionKey::new(ProviderId::Codex, format!("{state:?}")),
            process: ProcessPresence::Present,
            state,
            since_ms: None,
            raw_word: None,
            evidence: vec![],
            pid: None,
            console_id: None,
            active_subagents: 0,
            observed_at_ms: 0,
        }
    }

    #[test]
    fn counts_only_count_live_entries() {
        let snap = StatusSnapshot {
            generated_at_ms: 1,
            live: vec![
                obs(LiveState::Running),
                obs(LiveState::Running),
                obs(LiveState::NeedsYou),
            ],
        };
        assert_eq!(snap.counts(), (2, 1, 0));
    }

    #[test]
    fn two_polls_of_an_unchanged_machine_have_the_same_signature() {
        // The regression for a worker that emitted `status://changed` every five seconds forever:
        // whole-snapshot equality is defeated by the clock, and the View then replaced its status
        // and refetched the whole Live scope on every tick.
        let mut a = obs(LiveState::Running);
        let mut b = a.clone();
        a.observed_at_ms = 1_000;
        b.observed_at_ms = 9_999;
        let first = StatusSnapshot {
            generated_at_ms: 1,
            live: vec![a],
        };
        let second = StatusSnapshot {
            generated_at_ms: 500,
            live: vec![b],
        };
        assert_ne!(first, second, "the clock differs, as it always will");
        assert_eq!(first.signature(), second.signature(), "but nothing changed");
    }

    #[test]
    fn a_changed_state_changes_the_signature() {
        let running = StatusSnapshot {
            generated_at_ms: 1,
            live: vec![obs(LiveState::Running)],
        };
        let mut moved = obs(LiveState::Running);
        moved.state = LiveState::NeedsYou;
        let needs_you = StatusSnapshot {
            generated_at_ms: 1,
            live: vec![moved],
        };
        assert_ne!(running.signature(), needs_you.signature());
    }

    #[test]
    fn the_order_observations_arrive_in_is_not_a_change() {
        let a = obs(LiveState::Running);
        let b = obs(LiveState::NeedsYou);
        let one = StatusSnapshot {
            generated_at_ms: 1,
            live: vec![a.clone(), b.clone()],
        };
        let other = StatusSnapshot {
            generated_at_ms: 1,
            live: vec![b, a],
        };
        assert_eq!(one.signature(), other.signature());
    }

    #[test]
    fn an_empty_snapshot_counts_nothing() {
        assert_eq!(StatusSnapshot::default().counts(), (0, 0, 0));
    }

    #[test]
    fn delegating_is_an_active_session_state() {
        assert_eq!(
            SessionStatus::from(LiveState::Delegating),
            SessionStatus::Delegating
        );
        let snap = StatusSnapshot {
            generated_at_ms: 1,
            live: vec![obs(LiveState::Delegating)],
        };
        assert_eq!(snap.counts(), (1, 0, 0));
    }
}
