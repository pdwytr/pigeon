//! Persisted view preferences, in one small TOML file.
//!
//! It holds no credential, no transcript, no metric history and no process history — those are
//! either runtime state or somebody else's file. Out-of-range values revert to the default
//! rather than failing the load: a settings file the owner hand-edited badly should not stop the
//! app from starting.
//!
//! TOML is read and written by hand here. The file has four sections and nine scalars, and a
//! serializer crate for that would be a dependency bought for nothing.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_POLL_SECONDS: u32 = 5;
pub const POLL_SECONDS_RANGE: (u32, u32) = (1, 300);
pub const DEFAULT_RECENT_DAYS: u32 = 7;
pub const RECENT_DAYS_RANGE: (u32, u32) = (1, 365);
pub const DEFAULT_SPLIT: f64 = 0.38;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Corner {
    Tl,
    Tr,
    Bl,
    Br,
}

impl Corner {
    fn as_str(self) -> &'static str {
        match self {
            Corner::Tl => "tl",
            Corner::Tr => "tr",
            Corner::Bl => "bl",
            Corner::Br => "br",
        }
    }
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "tl" => Some(Corner::Tl),
            "tr" => Some(Corner::Tr),
            "bl" => Some(Corner::Bl),
            "br" => Some(Corner::Br),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HoverSettings {
    pub visible: bool,
    pub corner: Option<Corner>,
    pub x: Option<f64>,
    pub y: Option<f64>,
}

impl Default for HoverSettings {
    fn default() -> Self {
        Self {
            visible: false,
            corner: Some(Corner::Tr),
            x: None,
            y: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSettings {
    pub view: crate::api::types::Scope,
}

impl Default for ListSettings {
    fn default() -> Self {
        Self {
            view: crate::api::types::Scope::Live,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub hover: HoverSettings,
    pub list: ListSettings,
    pub poll_interval_seconds: u32,
    pub recent_window_days: u32,
    pub split: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hover: HoverSettings::default(),
            list: ListSettings::default(),
            poll_interval_seconds: DEFAULT_POLL_SECONDS,
            recent_window_days: DEFAULT_RECENT_DAYS,
            split: DEFAULT_SPLIT,
        }
    }
}

/// A patch from the View. Every field optional; absent means "leave it alone".
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub hover: Option<HoverSettings>,
    pub list: Option<ListSettings>,
    pub poll_interval_seconds: Option<u32>,
    pub recent_window_days: Option<u32>,
    pub split: Option<f64>,
}

impl Settings {
    /// Clamp every out-of-range value back to its default. An invalid setting is the owner's
    /// typo, not a reason to refuse to start.
    pub fn sanitized(mut self) -> Self {
        let (lo, hi) = POLL_SECONDS_RANGE;
        if self.poll_interval_seconds < lo || self.poll_interval_seconds > hi {
            self.poll_interval_seconds = DEFAULT_POLL_SECONDS;
        }
        let (lo, hi) = RECENT_DAYS_RANGE;
        if self.recent_window_days < lo || self.recent_window_days > hi {
            self.recent_window_days = DEFAULT_RECENT_DAYS;
        }
        if !self.split.is_finite() || self.split <= 0.05 || self.split >= 0.95 {
            self.split = DEFAULT_SPLIT;
        }
        self
    }

    pub fn apply(mut self, patch: SettingsPatch) -> Self {
        if let Some(hover) = patch.hover {
            self.hover = hover;
        }
        if let Some(list) = patch.list {
            self.list = list;
        }
        if let Some(v) = patch.poll_interval_seconds {
            self.poll_interval_seconds = v;
        }
        if let Some(v) = patch.recent_window_days {
            self.recent_window_days = v;
        }
        if let Some(v) = patch.split {
            self.split = v;
        }
        self.sanitized()
    }

    /// How far back `recent` reaches, as an epoch-ms cutoff.
    pub fn recent_cutoff_ms(&self, now_ms: i64) -> i64 {
        now_ms - (self.recent_window_days as i64) * 86_400_000
    }

    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text).sanitized(),
            // A missing file is the first run, not a problem.
            Err(_) => Self::default(),
        }
    }

    /// Write atomically: a temp file beside the target, then a rename.
    ///
    /// `fs::write` truncates and then writes, so a crash or two interleaved writers can leave a
    /// half-written config that the next launch reads as defaults. A rename on the same
    /// filesystem is atomic, so a reader sees either the old file or the new one and never a
    /// torn one. The temp name carries the pid so two processes cannot collide on it.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
        std::fs::write(&temp, self.to_toml())?;
        match std::fs::rename(&temp, path) {
            Ok(()) => Ok(()),
            Err(err) => {
                // Do not leave litter beside the owner's config when the rename fails.
                let _ = std::fs::remove_file(&temp);
                Err(err)
            }
        }
    }

    pub fn to_toml(&self) -> String {
        let scope = match self.list.view {
            crate::api::types::Scope::Live => "live",
            crate::api::types::Scope::Recent => "recent",
        };
        let corner = self.hover.corner.map(|c| c.as_str()).unwrap_or("tr");
        // An absent hover position is written as an ABSENT KEY, not as 0. The hover has never
        // been placed and a literal 0,0 would pin it to a corner the owner did not choose — and
        // would come back from `from_toml` as `Some(0.0)`, which is a different fact.
        let mut out = format!(
            "[view]\nscope = \"{scope}\"\nsplit = {split}\n\n\
             [polling]\ninterval_seconds = {poll}\nrecent_window_days = {days}\n\n\
             [hover]\nvisible = {visible}\ncorner = \"{corner}\"\n",
            split = self.split,
            poll = self.poll_interval_seconds,
            days = self.recent_window_days,
            visible = self.hover.visible,
        );
        if let Some(x) = self.hover.x {
            out.push_str(&format!("x = {x}\n"));
        }
        if let Some(y) = self.hover.y {
            out.push_str(&format!("y = {y}\n"));
        }
        out
    }

    /// A deliberately forgiving reader: it takes the keys it knows and ignores everything else,
    /// so a newer Pigeon's file does not break an older one.
    pub fn from_toml(text: &str) -> Self {
        let mut out = Settings::default();
        let mut section = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.trim().to_string();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match (section.as_str(), key) {
                ("view", "scope") => {
                    out.list.view = match value {
                        "recent" => crate::api::types::Scope::Recent,
                        _ => crate::api::types::Scope::Live,
                    }
                }
                ("view", "split") => {
                    if let Ok(v) = value.parse() {
                        out.split = v;
                    }
                }
                ("polling", "interval_seconds") => {
                    if let Ok(v) = value.parse() {
                        out.poll_interval_seconds = v;
                    }
                }
                ("polling", "recent_window_days") => {
                    if let Ok(v) = value.parse() {
                        out.recent_window_days = v;
                    }
                }
                ("hover", "visible") => out.hover.visible = value == "true",
                ("hover", "corner") => out.hover.corner = Corner::parse(value),
                ("hover", "x") => out.hover.x = value.parse().ok(),
                ("hover", "y") => out.hover.y = value.parse().ok(),
                _ => {}
            }
        }
        out
    }
}

/// Where the file lives. Beside the platform's config directory, under the app identifier.
pub fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("com.intanalytic.feather")
        .join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_contract() {
        let s = Settings::default();
        assert_eq!(s.poll_interval_seconds, 5);
        assert_eq!(s.recent_window_days, 7);
        assert_eq!(s.list.view, crate::api::types::Scope::Live);
        assert!(!s.hover.visible);
    }

    #[test]
    fn out_of_range_values_revert_to_defaults() {
        let bad = Settings {
            poll_interval_seconds: 0,
            recent_window_days: 4000,
            split: f64::NAN,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(bad.poll_interval_seconds, DEFAULT_POLL_SECONDS);
        assert_eq!(bad.recent_window_days, DEFAULT_RECENT_DAYS);
        assert_eq!(bad.split, DEFAULT_SPLIT);

        let too_big = Settings {
            poll_interval_seconds: 301,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(too_big.poll_interval_seconds, DEFAULT_POLL_SECONDS);
        // …and a value at the edge of the range is kept.
        let edge = Settings {
            poll_interval_seconds: 300,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(edge.poll_interval_seconds, 300);
    }

    #[test]
    fn a_patch_leaves_absent_fields_alone() {
        let base = Settings {
            split: 0.5,
            ..Default::default()
        };
        let patched = base.apply(SettingsPatch {
            recent_window_days: Some(30),
            ..Default::default()
        });
        assert_eq!(patched.recent_window_days, 30);
        assert_eq!(patched.split, 0.5, "an absent field was not touched");
    }

    #[test]
    fn toml_round_trips() {
        let original = Settings {
            hover: HoverSettings {
                visible: true,
                corner: Some(Corner::Bl),
                x: Some(12.0),
                y: Some(8.0),
            },
            list: ListSettings {
                view: crate::api::types::Scope::Recent,
            },
            poll_interval_seconds: 30,
            recent_window_days: 14,
            split: 0.42,
        };
        let back = Settings::from_toml(&original.to_toml());
        assert_eq!(back, original);
    }

    #[test]
    fn an_unknown_key_is_ignored_rather_than_fatal() {
        let text = "[view]\nscope = \"recent\"\nfuture_thing = 9\n\n[nonsense]\nx = 1\n";
        let s = Settings::from_toml(text);
        assert_eq!(s.list.view, crate::api::types::Scope::Recent);
        assert_eq!(s.poll_interval_seconds, DEFAULT_POLL_SECONDS);
    }

    #[test]
    fn a_missing_file_is_the_first_run_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nope").join("config.toml");
        assert_eq!(Settings::load(&path), Settings::default());
        assert!(!path.exists(), "loading must not create the file");
    }

    #[test]
    fn an_unplaced_hover_stays_unplaced_rather_than_becoming_zero_zero() {
        let s = Settings::default();
        assert_eq!(s.hover.x, None);
        let text = s.to_toml();
        assert!(
            !text.contains("x ="),
            "an absent position writes no key: {text}"
        );
        assert_eq!(Settings::from_toml(&text).hover.x, None);
    }

    #[test]
    fn save_then_load_survives_a_round_trip_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cfg").join("config.toml");
        let s = Settings {
            recent_window_days: 21,
            ..Default::default()
        };
        s.save(&path).expect("saves");
        assert_eq!(Settings::load(&path), s);
    }

    #[test]
    fn a_save_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        Settings::default().save(&path).expect("saves");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readable")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "config.toml")
            .collect();
        assert!(
            leftovers.is_empty(),
            "litter beside the config: {leftovers:?}"
        );
    }

    #[test]
    fn the_recent_cutoff_is_the_window_in_whole_days() {
        let s = Settings {
            recent_window_days: 7,
            ..Default::default()
        };
        let now = 1_000_000_000_000i64;
        assert_eq!(s.recent_cutoff_ms(now), now - 7 * 86_400_000);
    }
}
