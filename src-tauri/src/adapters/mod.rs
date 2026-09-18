//! The engine adapter boundary.
//!
//! **The contract's acceptance test is that nothing below this line changes when an engine is
//! added or removed.** An adapter knows about rollout files, JSONL records and SQLite columns;
//! the services above know only [`SessionCandidate`], [`Identity`], [`Capacity`] and
//! [`EngineError`].
//!
//! Invariant, inherited from Demo Studio and proven necessary twice there: **fail loud on an
//! unknown shape.** An adapter that meets a record it does not recognise records it in
//! [`Diagnostics`] or returns [`ErrorKind::UnknownShape`]. It never renders a number derived from
//! a guessed schema.

pub mod claude;
pub mod codex;
pub mod opencode;
pub mod wait;

pub use wait::{OwnerWait, WaitPolicy, WaitSignal};

use std::ffi::OsString;
use std::path::PathBuf;

use crate::api::errors::EngineError;
use crate::domain::{
    Capacity, Diagnostics, Identity, ProviderId, ResumeBlockedReason, SessionKey, SourceSignature,
    SourceSummary,
};

/// What one engine found. A `problem` travels *with* the rows so one broken engine never blanks
/// the merged list.
#[derive(Debug, Default)]
pub struct ProviderSessionReport {
    pub sessions: Vec<SessionCandidate>,
    pub problem: Option<EngineError>,
}

impl ProviderSessionReport {
    pub fn problem(error: EngineError) -> Self {
        Self {
            sessions: vec![],
            problem: Some(error),
        }
    }
}

/// A neutral draft session. The session service applies project normalization, scope filtering,
/// sorting and account association; the adapter does none of that.
#[derive(Clone, Debug)]
pub struct SessionCandidate {
    pub key: SessionKey,
    pub cwd: Option<PathBuf>,
    pub title: String,
    pub name: Option<String>,
    pub git_branch: Option<String>,
    pub first_active_ms: Option<i64>,
    pub last_active_ms: i64,
    pub closed_at_ms: Option<i64>,
    pub resumable: bool,
    pub resume_blocked_reason: Option<ResumeBlockedReason>,
    pub source: SourceSummary,
    pub diagnostics: Diagnostics,
    pub source_signature: SourceSignature,
}

/// The engine's own resume invocation. The program is resolved on PATH at spawn time; Pigeon
/// never bundles, vendors or redistributes an engine CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeCommand {
    pub program: String,
    pub args: Vec<OsString>,
}

impl ResumeCommand {
    pub fn new(provider: ProviderId, sid: &str) -> Self {
        Self {
            program: provider.program().to_string(),
            args: provider
                .resume_args(sid)
                .into_iter()
                .map(OsString::from)
                .collect(),
        }
    }
}

/// Every engine implements this and nothing else is visible to the services above.
pub trait ProviderAdapter: Send + Sync {
    fn provider(&self) -> ProviderId;

    /// Every session this engine has on this machine, read-only. Never mutates, moves or locks
    /// a file the engine owns.
    fn discover_sessions(&self) -> ProviderSessionReport;

    /// Who is signed in. Safe identity fields only.
    fn read_identity(&self) -> Identity;

    /// How much allowance is left. An engine that publishes none returns
    /// [`Capacity::unsupported`] — an absence, not an `Err`.
    fn read_capacity(&self) -> Capacity;

    /// How to resume one session.
    fn resume_command(&self, sid: &str) -> Result<ResumeCommand, EngineError> {
        if sid.trim().is_empty() {
            return Err(EngineError::of(
                self.provider(),
                crate::api::errors::ErrorKind::Path,
            ));
        }
        Ok(ResumeCommand::new(self.provider(), sid))
    }
}

/// The three adapters, in a stable order.
pub fn all() -> Vec<Box<dyn ProviderAdapter>> {
    vec![
        Box::new(claude::ClaudeAdapter::new()),
        Box::new(codex::CodexAdapter::new()),
        Box::new(opencode::OpenCodeAdapter::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_commands_are_each_engines_own() {
        let c = ResumeCommand::new(ProviderId::ClaudeCode, "sid-1");
        assert_eq!(c.program, "claude");
        assert_eq!(
            c.args,
            vec![OsString::from("--resume"), OsString::from("sid-1")]
        );

        let x = ResumeCommand::new(ProviderId::Codex, "sid-2");
        assert_eq!(x.program, "codex");
        assert_eq!(
            x.args,
            vec![OsString::from("resume"), OsString::from("sid-2")]
        );

        let o = ResumeCommand::new(ProviderId::OpenCode, "sid-3");
        assert_eq!(o.program, "opencode");
        assert_eq!(
            o.args,
            vec![OsString::from("--session"), OsString::from("sid-3")]
        );
    }

    #[test]
    fn a_whole_sid_is_passed_through_never_truncated() {
        let sid = "0199c4a1-2b3d-7e4f-8a9b-0c1d2e3f4a5b";
        let c = ResumeCommand::new(ProviderId::Codex, sid);
        assert_eq!(c.args[1], OsString::from(sid));
    }
}
