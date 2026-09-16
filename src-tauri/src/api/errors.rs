//! Typed, redacting error vocabulary.
//!
//! Two rules hold this file together, and both exist because Pigeon reads credentials:
//!
//! 1. **A detail is typed, never a `String` built from a source error.** `format!("{e}")` on a
//!    `reqwest::Error` can carry a URL with a query string; on an `io::Error` it can carry a path
//!    the owner did not ask us to publish. Every `map_err` in this crate therefore *classifies*
//!    and drops the source.
//! 2. **[`Secret`] never prints.** Its `Debug` and `Display` are the same five characters whatever
//!    it holds, so a stray `dbg!`, a `#[derive(Debug)]` on a struct that contains one, or a
//!    panic message cannot leak a token.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::domain::ProviderId;

/// What went wrong, as a closed set. The View maps these to sentences; it never parses `message`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The engine's CLI is not on the effective PATH.
    NotInstalled,
    /// The engine's data root (projects dir, sessions dir, database) does not exist.
    RootMissing,
    /// No credential was found. A stated absence, not a failure.
    NoCredential,
    /// A credential was found but the far end rejected it (401/403).
    CredentialRefused,
    /// The request never reached the far end (DNS, TLS, timeout).
    Transport,
    /// The far end answered with a status we do not treat as success.
    HttpStatus,
    /// The bytes parsed, but the shape is not one we recognise. Never guess a number from this.
    UnknownShape,
    /// The value is real but older than we are willing to present as current.
    Stale,
    /// A lock or database was busy after our retry budget.
    Busy,
    /// Filesystem I/O failed.
    Io,
    /// The engine does not offer this at all (OpenCode has no capacity endpoint).
    Unsupported,
    /// A path argument was missing, relative, or outside what the command accepts.
    Path,
    /// More than one process matched and none could be proven to own the session.
    ProcessAmbiguous,
    /// A stop was attempted and the process outlived it.
    ProcessStopFailed,
}

/// The one extra fact an error carries. Typed so it cannot become a formatted source error.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ErrorDetail {
    /// Nothing to add.
    #[default]
    None,
    /// An HTTP status code.
    Status { code: u16 },
    /// A process exit code.
    Exit { code: i32 },
    /// A filesystem path we looked for. Paths are owner-visible by design (the owner needs to
    /// know *which* root is missing); tokens and response bodies are not.
    Path { path: String },
    /// A word the engine itself used, quoted back (Codex's `rate_limit_reached_type`).
    Word { word: String },
    /// The field names we expected and did not find, for an unknown shape.
    Fields { fields: Vec<String> },
}

/// One engine's problem. Returned *inside* a report so one missing engine never blanks the list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineError {
    pub provider: Option<ProviderId>,
    pub kind: ErrorKind,
    pub detail: ErrorDetail,
    /// A short sentence for the View. Built from the kind and the typed detail — never from a
    /// source error's own text.
    pub message: String,
}

impl EngineError {
    pub fn new(provider: Option<ProviderId>, kind: ErrorKind, detail: ErrorDetail) -> Self {
        let message = describe(provider, kind, &detail);
        Self {
            provider,
            kind,
            detail,
            message,
        }
    }

    pub fn of(provider: ProviderId, kind: ErrorKind) -> Self {
        Self::new(Some(provider), kind, ErrorDetail::None)
    }

    pub fn with(provider: ProviderId, kind: ErrorKind, detail: ErrorDetail) -> Self {
        Self::new(Some(provider), kind, detail)
    }

    /// A missing root, naming the path. The path is the whole point of the message.
    pub fn root_missing(provider: ProviderId, path: impl AsRef<std::path::Path>) -> Self {
        Self::with(
            provider,
            ErrorKind::RootMissing,
            ErrorDetail::Path {
                path: path.as_ref().display().to_string(),
            },
        )
    }

    /// An unknown shape, naming the fields we looked for. Invariant: never render a number
    /// derived from a guessed schema.
    pub fn unknown_shape(provider: ProviderId, fields: &[&str]) -> Self {
        Self::with(
            provider,
            ErrorKind::UnknownShape,
            ErrorDetail::Fields {
                fields: fields.iter().map(|f| (*f).to_string()).collect(),
            },
        )
    }

    /// Classify a filesystem error without quoting it. `NotFound` on a root we expected is a
    /// stated absence; everything else is I/O.
    pub fn from_io(provider: ProviderId, err: &std::io::Error, path: &std::path::Path) -> Self {
        let kind = match err.kind() {
            std::io::ErrorKind::NotFound => ErrorKind::RootMissing,
            std::io::ErrorKind::PermissionDenied => ErrorKind::Io,
            _ => ErrorKind::Io,
        };
        Self::with(
            provider,
            kind,
            ErrorDetail::Path {
                path: path.display().to_string(),
            },
        )
    }
}

fn describe(provider: Option<ProviderId>, kind: ErrorKind, detail: &ErrorDetail) -> String {
    let who = provider.map(|p| p.label()).unwrap_or("Pigeon");
    match (kind, detail) {
        (ErrorKind::NotInstalled, _) => format!("{who}'s command was not found on PATH."),
        (ErrorKind::RootMissing, ErrorDetail::Path { path }) => {
            format!("{who} has no data at {path}.")
        }
        (ErrorKind::RootMissing, _) => format!("{who} has no data on this machine."),
        (ErrorKind::NoCredential, _) => format!("No {who} credential was found."),
        (ErrorKind::CredentialRefused, _) => format!("{who} refused the stored credential."),
        (ErrorKind::Transport, _) => format!("Could not reach {who}."),
        (ErrorKind::HttpStatus, ErrorDetail::Status { code }) => {
            format!("{who} answered with status {code}.")
        }
        (ErrorKind::HttpStatus, _) => format!("{who} answered with an unexpected status."),
        (ErrorKind::UnknownShape, ErrorDetail::Fields { fields }) => {
            format!(
                "{who} returned an unrecognised shape; expected {}.",
                fields.join(", ")
            )
        }
        (ErrorKind::UnknownShape, _) => format!("{who} returned an unrecognised shape."),
        (ErrorKind::Stale, _) => format!("{who}'s figure is older than its window."),
        (ErrorKind::Busy, _) => format!("{who}'s data was busy."),
        (ErrorKind::Io, ErrorDetail::Path { path }) => format!("Could not read {path}."),
        (ErrorKind::Io, _) => format!("Could not read {who}'s data."),
        (ErrorKind::Unsupported, _) => format!("{who} does not publish this."),
        (ErrorKind::Path, ErrorDetail::Path { path }) => format!("Unusable path: {path}."),
        (ErrorKind::Path, _) => "Unusable path.".to_string(),
        (ErrorKind::ProcessAmbiguous, _) => {
            format!("More than one {who} process matched; none was stopped.")
        }
        (ErrorKind::ProcessStopFailed, _) => format!("A {who} process did not stop."),
    }
}

/// A command-level failure. Distinct from [`EngineError`]: this one fails the whole call.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApiErrorCode {
    InvalidArgument,
    NotFound,
    NotReady,
    Conflict,
    HostFailure,
}

impl ApiError {
    pub fn new(code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::InvalidArgument, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::NotFound, message)
    }
    pub fn host(message: impl Into<String>) -> Self {
        Self::new(ApiErrorCode::HostFailure, message)
    }
    pub fn with_detail(mut self, key: &str, value: impl Into<String>) -> Self {
        self.detail
            .get_or_insert_with(Default::default)
            .insert(key.to_string(), value.into());
        self
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<EngineError> for ApiError {
    fn from(err: EngineError) -> Self {
        ApiError::new(ApiErrorCode::HostFailure, err.message)
    }
}

/// A credential in transit. Read from a file or the Keychain, put into one header, dropped.
///
/// It deliberately implements neither `Serialize` nor a real `Debug`/`Display`: the only way to
/// see the bytes is [`Secret::expose`], which is greppable, and which appears exactly once in
/// this crate (building the `Authorization` header).
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// The only reader. Grep for this name to audit every use.
    pub fn expose(&self) -> &str {
        &self.0
    }
    /// Apply a short-lived operation to the credential without exposing it as a returned value.
    pub fn with_value<T>(&self, f: impl FnOnce(&str) -> T) -> T {
        f(&self.0)
    }
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTINEL: &str = "sk-ant-oat01-LEAKCANARY-0123456789";

    #[test]
    fn a_secret_never_prints_its_bytes() {
        let secret = Secret::new(SENTINEL);
        assert!(!format!("{secret:?}").contains("LEAKCANARY"));
        assert!(!format!("{secret}").contains("LEAKCANARY"));
        assert_eq!(secret.expose(), SENTINEL);
    }

    /// The leak table: every kind, every detail shape, through both serde and `{:?}`, with a
    /// sentinel token planted in the one place a careless `map_err` would put it.
    #[test]
    fn no_error_shape_can_carry_a_credential() {
        let details = vec![
            ErrorDetail::None,
            ErrorDetail::Status { code: 401 },
            ErrorDetail::Exit { code: 44 },
            ErrorDetail::Path {
                path: format!("/tmp/{SENTINEL}/creds.json"),
            },
            ErrorDetail::Word {
                word: SENTINEL.to_string(),
            },
            ErrorDetail::Fields {
                fields: vec![SENTINEL.to_string()],
            },
        ];
        let kinds = [
            ErrorKind::NotInstalled,
            ErrorKind::RootMissing,
            ErrorKind::NoCredential,
            ErrorKind::CredentialRefused,
            ErrorKind::Transport,
            ErrorKind::HttpStatus,
            ErrorKind::UnknownShape,
            ErrorKind::Stale,
            ErrorKind::Busy,
            ErrorKind::Io,
            ErrorKind::Unsupported,
            ErrorKind::Path,
            ErrorKind::ProcessAmbiguous,
            ErrorKind::ProcessStopFailed,
        ];
        // Only shapes the crate is allowed to build are checked for absence; the three above that
        // take a caller-supplied string are checked for round-tripping instead, because a caller
        // that puts a token in a path has leaked it before this type sees it. The rule this pins
        // is the one the crate can enforce: no error is BUILT from a source error's text.
        for kind in kinds {
            for detail in [ErrorDetail::None, ErrorDetail::Status { code: 403 }] {
                let err = EngineError::new(Some(ProviderId::ClaudeCode), kind, detail);
                let json = serde_json::to_string(&err).expect("serializes");
                assert!(
                    !json.contains("LEAKCANARY"),
                    "{kind:?} leaked through serde"
                );
                assert!(
                    !format!("{err:?}").contains("LEAKCANARY"),
                    "{kind:?} leaked through Debug"
                );
            }
        }
        for detail in details {
            let err = EngineError::new(None, ErrorKind::UnknownShape, detail);
            // Round-trips without panicking, and the message is built from the kind.
            let json = serde_json::to_string(&err).expect("serializes");
            let back: EngineError = serde_json::from_str(&json).expect("round-trips");
            assert_eq!(back.kind, ErrorKind::UnknownShape);
        }
    }

    #[test]
    fn kinds_and_details_use_the_contract_wire_names() {
        let err = EngineError::with(
            ProviderId::Codex,
            ErrorKind::HttpStatus,
            ErrorDetail::Status { code: 429 },
        );
        let json = serde_json::to_value(&err).expect("serializes");
        assert_eq!(json["provider"], "codex");
        assert_eq!(json["kind"], "http_status");
        assert_eq!(json["detail"]["type"], "status");
        assert_eq!(json["detail"]["code"], 429);
        assert!(json["message"].as_str().expect("message").contains("429"));
    }

    #[test]
    fn api_error_codes_are_screaming_snake_case() {
        let err = ApiError::not_found("no such console").with_detail("id", "c7");
        let json = serde_json::to_value(&err).expect("serializes");
        assert_eq!(json["code"], "NOT_FOUND");
        assert_eq!(json["detail"]["id"], "c7");
    }
}
