//! Event topics and payloads. The topic strings are frozen seam: the View subscribes to these
//! exact names, so they are constants here rather than literals at each emit site.

use serde::{Deserialize, Serialize};

use crate::api::types::{AccountStatusDto, MetricStateDto, Scope};
use crate::domain::{ProviderId, SessionKey};

/// A scope's session list changed. The View refetches that scope.
pub const SESSIONS_CHANGED: &str = "sessions://changed";
/// A batch of lazily folded metrics landed. At most [`METRICS_BATCH`] rows per event.
pub const SESSIONS_METRICS: &str = "sessions://metrics";
/// The live-status projection changed. Replaces the snapshot atomically.
pub const STATUS_CHANGED: &str = "status://changed";
/// One engine's account/capacity changed.
pub const CAPACITY_CHANGED: &str = "capacity://changed";
/// PTY output, in order.
pub const CONSOLE_DATA: &str = "console://data";
/// A console's child ended. Emitted exactly once per console.
pub const CONSOLE_EXIT: &str = "console://exit";
/// The hover window asked the main window to select a session.
pub const SELECT_SESSION: &str = "pigeon://select-session";

/// Metric events arrive in batches of at most this many rows.
pub const METRICS_BATCH: usize = 20;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsChanged {
    pub scope: Scope,
    pub generated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsRow {
    pub key: SessionKey,
    pub metrics: MetricStateDto,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsMetrics {
    pub rows: Vec<MetricsRow>,
    pub generated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityChanged {
    pub provider: ProviderId,
    pub account: AccountStatusDto,
    pub generated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleData {
    pub id: String,
    pub data_b64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleExit {
    pub id: String,
    pub exit_code: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_are_the_frozen_names() {
        assert_eq!(SESSIONS_CHANGED, "sessions://changed");
        assert_eq!(SESSIONS_METRICS, "sessions://metrics");
        assert_eq!(STATUS_CHANGED, "status://changed");
        assert_eq!(CAPACITY_CHANGED, "capacity://changed");
        assert_eq!(CONSOLE_DATA, "console://data");
        assert_eq!(CONSOLE_EXIT, "console://exit");
        assert_eq!(SELECT_SESSION, "pigeon://select-session");
    }

    #[test]
    fn console_payload_keys_are_camel_case() {
        let data = ConsoleData {
            id: "c1".into(),
            data_b64: "aGk=".into(),
        };
        let json = serde_json::to_value(&data).expect("serializes");
        assert_eq!(json["dataB64"], "aGk=");
        let exit = ConsoleExit {
            id: "c1".into(),
            exit_code: None,
        };
        let json = serde_json::to_value(&exit).expect("serializes");
        assert!(json["exitCode"].is_null());
    }

    #[test]
    fn a_metrics_batch_names_the_whole_key() {
        let row = MetricsRow {
            key: SessionKey::new(ProviderId::OpenCode, "s-1"),
            metrics: MetricStateDto::Pending,
        };
        let json = serde_json::to_value(&SessionsMetrics {
            rows: vec![row],
            generated_at_ms: 3,
        })
        .expect("serializes");
        assert_eq!(json["rows"][0]["key"]["providerId"], "opencode");
        assert_eq!(json["rows"][0]["key"]["sid"], "s-1");
        assert_eq!(json["generatedAtMs"], 3);
    }
}
