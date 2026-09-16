//! Who is signed in, and how much of each allowance is left.
//!
//! No field here holds a credential. The token is read, put into one header, and dropped inside
//! the capacity reader; it never reaches a struct that is serialized, logged or cached.

use serde::{Deserialize, Serialize};

use super::ProviderId;
use crate::api::errors::EngineError;

#[derive(Clone, Debug)]
pub struct AccountStatus {
    pub provider: ProviderId,
    pub identity: Identity,
    pub capacity: Capacity,
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub provider: ProviderId,
    pub signed_in: bool,
    /// Email or display name. Safe identity, never a token.
    pub label: Option<String>,
    pub organization: Option<String>,
    pub plan: Option<String>,
    pub tier: Option<String>,
    /// Codex's `auth_mode` — how the owner signed in, not what with.
    pub mode: Option<String>,
    /// A bounded tail of an account id, for telling two logins apart. Never the whole id.
    pub account_short: Option<String>,
    /// OpenCode's configured providers: names and kinds only.
    pub providers: Option<Vec<ProviderSummary>>,
    pub read_at_ms: i64,
    pub problem: Option<EngineError>,
}

impl Identity {
    pub fn absent(provider: ProviderId, read_at_ms: i64, problem: Option<EngineError>) -> Self {
        Self {
            provider,
            signed_in: false,
            label: None,
            organization: None,
            plan: None,
            tier: None,
            mode: None,
            account_short: None,
            providers: None,
            read_at_ms,
            problem,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug)]
pub struct Capacity {
    pub provider: ProviderId,
    /// False means the engine publishes no allowance at all. The View renders "n/a" — never a
    /// zero bar, which would read as "all used".
    pub supported: bool,
    pub windows: Vec<CapacityWindow>,
    pub plan: Option<String>,
    /// The figure is real but older than its own window; shown with a caveat.
    pub stale: bool,
    pub source_age_s: Option<u64>,
    /// The engine's own word for a limit it has hit.
    pub reached_limit: Option<String>,
    pub read_at_ms: i64,
    pub problem: Option<EngineError>,
}

impl Capacity {
    pub fn unsupported(provider: ProviderId, read_at_ms: i64) -> Self {
        Self {
            provider,
            supported: false,
            windows: vec![],
            plan: None,
            stale: false,
            source_age_s: None,
            reached_limit: None,
            read_at_ms,
            problem: None,
        }
    }

    pub fn problem(provider: ProviderId, read_at_ms: i64, error: EngineError) -> Self {
        Self {
            problem: Some(error),
            ..Self::unsupported(provider, read_at_ms)
        }
    }
}

/// Which allowance. Identified by `window_minutes` from the engine, never by position in a list.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityWindowName {
    FiveHour,
    Weekly,
    Monthly,
}

impl CapacityWindowName {
    /// 300 minutes is the five-hour window, 10080 the week. Anything else is unrecognised and
    /// is dropped rather than guessed into a slot.
    pub fn from_minutes(minutes: u32) -> Option<Self> {
        match minutes {
            300 => Some(CapacityWindowName::FiveHour),
            10080 => Some(CapacityWindowName::Weekly),
            43200 => Some(CapacityWindowName::Monthly),
            _ => None,
        }
    }

    pub fn minutes(self) -> u32 {
        match self {
            CapacityWindowName::FiveHour => 300,
            CapacityWindowName::Weekly => 10080,
            CapacityWindowName::Monthly => 43200,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityWindow {
    pub name: CapacityWindowName,
    pub window_minutes: u32,
    pub used_pct: f64,
    pub resets_at_ms: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_identified_by_minutes_not_by_slot() {
        assert_eq!(
            CapacityWindowName::from_minutes(300),
            Some(CapacityWindowName::FiveHour)
        );
        assert_eq!(
            CapacityWindowName::from_minutes(10080),
            Some(CapacityWindowName::Weekly)
        );
        assert_eq!(CapacityWindowName::from_minutes(60), None);
        assert_eq!(CapacityWindowName::from_minutes(0), None);
    }

    #[test]
    fn unsupported_capacity_has_no_windows_to_draw() {
        let cap = Capacity::unsupported(ProviderId::OpenCode, 10);
        assert!(!cap.supported);
        assert!(cap.windows.is_empty());
        assert!(cap.problem.is_none());
    }
}
