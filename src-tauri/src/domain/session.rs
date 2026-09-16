//! Session identity, project identity, and the neutral session record.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::api::errors::EngineError;

/// The reserved project key for a session whose source record names no working directory.
pub const NO_DIRECTORY: &str = "__no_directory__";

/// Which engine a session belongs to. The *only* provider discriminator outside an adapter.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderId {
    ClaudeCode,
    Codex,
    /// Spelled `opencode` on the wire, not the `open-code` that `kebab-case` would produce —
    /// the engine's own name is one word. Pinned by
    /// `provider_wire_values_match_the_contract`.
    #[serde(rename = "opencode")]
    OpenCode,
}

impl ProviderId {
    pub const ALL: [ProviderId; 3] = [
        ProviderId::ClaudeCode,
        ProviderId::Codex,
        ProviderId::OpenCode,
    ];

    /// The wire value. Matches the serde rename, and is pinned by a test.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::ClaudeCode => "claude-code",
            ProviderId::Codex => "codex",
            ProviderId::OpenCode => "opencode",
        }
    }

    /// A human label for a message. Never parsed.
    pub fn label(self) -> &'static str {
        match self {
            ProviderId::ClaudeCode => "Claude Code",
            ProviderId::Codex => "Codex",
            ProviderId::OpenCode => "OpenCode",
        }
    }

    /// The command this engine is launched by, resolved on PATH at spawn time and never bundled.
    pub fn program(self) -> &'static str {
        match self {
            ProviderId::ClaudeCode => "claude",
            ProviderId::Codex => "codex",
            ProviderId::OpenCode => "opencode",
        }
    }

    /// The engine's own resume invocation, as argv tail. The session id is passed whole.
    pub fn resume_args(self, sid: &str) -> Vec<String> {
        match self {
            ProviderId::ClaudeCode => vec!["--resume".into(), sid.to_string()],
            ProviderId::Codex => vec!["resume".into(), sid.to_string()],
            ProviderId::OpenCode => vec!["--session".into(), sid.to_string()],
        }
    }
}

/// A session's complete identity. `sid` is never used alone — not as a map key, a React key, a
/// cache key, or a command argument. Two engines may legitimately mint the same uuid.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionKey {
    pub provider_id: ProviderId,
    pub sid: String,
}

impl SessionKey {
    pub fn new(provider_id: ProviderId, sid: impl Into<String>) -> Self {
        Self {
            provider_id,
            sid: sid.into(),
        }
    }

    /// Whitespace-only or empty ids are invalid — a provider that hands us one has drifted.
    pub fn is_valid(&self) -> bool {
        !self.sid.trim().is_empty()
    }

    /// The canonical string form used for cache maps and for the View's `sessionKeyId`.
    pub fn id(&self) -> String {
        format!("{}:{}", self.provider_id.as_str(), self.sid)
    }
}

/// A normalized absolute working directory, or [`NO_DIRECTORY`].
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProjectKey(pub String);

impl ProjectKey {
    /// The reserved key for a session with no usable working directory.
    pub fn none() -> Self {
        ProjectKey(NO_DIRECTORY.to_string())
    }

    pub fn is_none(&self) -> bool {
        self.0 == NO_DIRECTORY
    }

    /// Normalize a working directory into a stable key.
    ///
    /// Trailing separators go, `.` segments collapse, `..` pops, and the result is case-folded
    /// on Windows only. A relative path is kept as given rather than resolved against Pigeon's
    /// own cwd, which would attribute an engine's session to whatever folder Pigeon was launched
    /// from. Symlinks are deliberately *not* resolved: two engines that both record
    /// `/Users/x/proj` must land on one key, and canonicalizing would also touch the filesystem
    /// on a path that may no longer exist.
    pub fn normalize(cwd: Option<&Path>) -> Self {
        let Some(cwd) = cwd else { return Self::none() };
        let raw = cwd.to_string_lossy();
        if raw.trim().is_empty() {
            return Self::none();
        }
        let mut parts: Vec<String> = Vec::new();
        let is_absolute = raw.starts_with('/') || raw.starts_with('\\') || has_drive_prefix(&raw);
        for seg in raw.split(['/', '\\']) {
            match seg {
                "" | "." => {}
                ".." => {
                    if parts.last().map(|p| p != "..").unwrap_or(false) {
                        parts.pop();
                    } else if !is_absolute {
                        parts.push("..".to_string());
                    }
                }
                other => parts.push(other.to_string()),
            }
        }
        let joined = parts.join("/");
        let mut out = if is_absolute {
            format!("/{joined}")
        } else {
            joined
        };
        if out.is_empty() {
            out = "/".to_string();
        }
        if cfg!(windows) {
            out = out.to_lowercase();
        }
        ProjectKey(out)
    }

    /// The last path component, used as the display name.
    pub fn leaf(&self) -> String {
        if self.is_none() {
            return "(no directory)".to_string();
        }
        self.0
            .rsplit('/')
            .find(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.0.clone())
    }

    /// The path shown to the owner. The reserved key displays as a sentence, not a token.
    pub fn display_path(&self) -> String {
        if self.is_none() {
            "(no directory)".to_string()
        } else {
            self.0.clone()
        }
    }
}

fn has_drive_prefix(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && (bytes[0] as char).is_ascii_alphabetic()
}

/// Why a session cannot be resumed. Converted to a safe sentence at the API boundary; it never
/// carries command output or a provider's own error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeBlockedReason {
    /// The source record names no working directory, so there is nowhere to launch.
    MissingWorkingDirectory,
    /// The recorded working directory no longer exists on disk.
    WorkingDirectoryMissing,
    /// The engine's CLI is not on the effective PATH.
    ProviderNotInstalled,
    /// The session id is empty or malformed.
    InvalidSessionId,
}

impl ResumeBlockedReason {
    pub fn message(self) -> &'static str {
        match self {
            ResumeBlockedReason::MissingWorkingDirectory => {
                "This session records no working directory, so it cannot be resumed."
            }
            ResumeBlockedReason::WorkingDirectoryMissing => {
                "The folder this session ran in no longer exists."
            }
            ResumeBlockedReason::ProviderNotInstalled => {
                "This engine's command was not found on PATH."
            }
            ResumeBlockedReason::InvalidSessionId => "This session has no usable id.",
        }
    }
}

/// Which files a session was read from. A bounded projection — never transcript text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceSummary {
    pub paths: Vec<PathBuf>,
    pub files: u32,
}

impl SourceSummary {
    pub fn single(path: PathBuf) -> Self {
        Self {
            paths: vec![path],
            files: 1,
        }
    }

    pub fn many(paths: Vec<PathBuf>) -> Self {
        let files = paths.len() as u32;
        Self { paths, files }
    }

    /// One bounded line for the detail pane: the newest path plus a count when there are more.
    pub fn display(&self) -> Option<String> {
        let first = self.paths.first()?;
        Some(if self.files > 1 {
            format!("{} (+{} more)", first.display(), self.files - 1)
        } else {
            first.display().to_string()
        })
    }
}

/// Record types an adapter saw and did not recognise, with counts. Surfaced rather than ignored:
/// engine log formats drift, and a silent skip is how a wrong number gets rendered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub unknown_types: BTreeMap<String, u32>,
}

impl Diagnostics {
    pub fn note(&mut self, kind: &str) {
        *self.unknown_types.entry(kind.to_string()).or_insert(0) += 1;
    }
    pub fn is_empty(&self) -> bool {
        self.unknown_types.is_empty()
    }
}

/// Runtime cache identity for a session's source. Not persisted, never sent to the View.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceSignature {
    Claude {
        main: FileSignature,
        sidecar_mtime_ms: Option<i64>,
    },
    Codex {
        files: Vec<FileSignature>,
    },
    OpenCode {
        time_updated_ms: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSignature {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ms: i64,
}

impl FileSignature {
    /// Read a signature from a path. An unreadable path is an absence, not a panic.
    pub fn read(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            path: path.to_path_buf(),
            size: meta.len(),
            mtime_ms: crate::util::mtime_ms(&meta),
        })
    }
}

/// The neutral session record. Carries no process status — that is joined from
/// [`crate::domain::LiveObservation`] by [`SessionKey`].
#[derive(Clone, Debug)]
pub struct Session {
    pub key: SessionKey,
    pub cwd: Option<PathBuf>,
    pub project: ProjectKey,
    pub title: String,
    pub name: Option<String>,
    pub git_branch: Option<String>,
    pub first_active_ms: Option<i64>,
    pub last_active_ms: i64,
    pub closed_at_ms: Option<i64>,
    pub resumable: bool,
    pub resume_blocked_reason: Option<ResumeBlockedReason>,
    pub metrics: crate::domain::MetricState,
    pub source: SourceSummary,
    pub diagnostics: Diagnostics,
    pub signature: Option<SourceSignature>,
}

impl Session {
    /// The fallback title. A session with no user text yet is still a real session.
    pub const UNTITLED: &'static str = "Untitled session";

    pub fn problem(&self) -> Option<&EngineError> {
        match &self.metrics {
            crate::domain::MetricState::Unavailable { error } => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_wire_values_match_the_contract() {
        for (provider, wire) in [
            (ProviderId::ClaudeCode, "claude-code"),
            (ProviderId::Codex, "codex"),
            (ProviderId::OpenCode, "opencode"),
        ] {
            assert_eq!(provider.as_str(), wire);
            assert_eq!(serde_json::to_value(provider).expect("serializes"), wire);
        }
    }

    #[test]
    fn resume_args_are_each_engines_own_command() {
        assert_eq!(
            ProviderId::ClaudeCode.resume_args("abc"),
            vec!["--resume", "abc"]
        );
        assert_eq!(ProviderId::Codex.resume_args("abc"), vec!["resume", "abc"]);
        assert_eq!(
            ProviderId::OpenCode.resume_args("abc"),
            vec!["--session", "abc"]
        );
    }

    #[test]
    fn two_engines_sharing_a_sid_are_two_sessions() {
        let a = SessionKey::new(ProviderId::ClaudeCode, "same-uuid");
        let b = SessionKey::new(ProviderId::Codex, "same-uuid");
        assert_ne!(a, b);
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn an_empty_sid_is_invalid() {
        assert!(!SessionKey::new(ProviderId::Codex, "   ").is_valid());
        assert!(SessionKey::new(ProviderId::Codex, "x").is_valid());
    }

    #[test]
    fn a_trailing_slash_joins_the_same_project() {
        let a = ProjectKey::normalize(Some(Path::new("/Users/k/proj")));
        let b = ProjectKey::normalize(Some(Path::new("/Users/k/proj/")));
        let c = ProjectKey::normalize(Some(Path::new("/Users/k/./proj")));
        let d = ProjectKey::normalize(Some(Path::new("/Users/k/other/../proj")));
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a, d);
        assert_eq!(a.leaf(), "proj");
    }

    #[test]
    fn a_missing_directory_takes_the_reserved_key() {
        let key = ProjectKey::normalize(None);
        assert!(key.is_none());
        assert_eq!(key.0, NO_DIRECTORY);
        assert_eq!(key.leaf(), "(no directory)");
        assert_eq!(
            ProjectKey::normalize(Some(Path::new("  "))),
            ProjectKey::none()
        );
    }

    #[test]
    fn source_summary_display_is_bounded() {
        let one = SourceSummary::single(PathBuf::from("/a/b.jsonl"));
        assert_eq!(one.display().as_deref(), Some("/a/b.jsonl"));
        let many = SourceSummary::many(vec![
            PathBuf::from("/a/b.jsonl"),
            PathBuf::from("/a/c.jsonl"),
        ]);
        assert_eq!(many.display().as_deref(), Some("/a/b.jsonl (+1 more)"));
        assert_eq!(SourceSummary::default().display(), None);
    }
}
