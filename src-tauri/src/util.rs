//! Small shared helpers. UTC epoch milliseconds everywhere inside Rust; the View formats.

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Now, as UTC epoch milliseconds.
pub fn now_ms() -> i64 {
    to_ms(SystemTime::now())
}

/// A `SystemTime` as UTC epoch milliseconds. Times before the epoch go negative rather than
/// saturating to 0, so a bad clock is visible instead of looking like 1970.
pub fn to_ms(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

/// A file's modification time in epoch ms, or 0 when the platform does not report one.
pub fn mtime_ms(meta: &Metadata) -> i64 {
    meta.modified().map(to_ms).unwrap_or(0)
}

/// True when `name` is a 36-character canonical UUID (8-4-4-4-12 hex). Claude names its
/// transcript files by session uuid; a file whose stem is not one is not a session.
pub fn is_uuid(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        let ok = match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Collapse whitespace and clip to `max` characters, appending an ellipsis when clipped.
/// Titles come from transcript text, so they can be a paragraph or contain newlines.
pub fn tidy_title(raw: &str, max: usize) -> String {
    let mut out = String::with_capacity(raw.len().min(max + 1));
    let mut last_space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    let trimmed = out.trim_end();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let clipped: String = trimmed.chars().take(max).collect();
    format!("{}…", clipped.trim_end())
}

/// The display name of the repository containing a working directory.
///
/// Prefer the origin repository name (so a nested cwd such as repo/app displays repo), then the
/// local Git root. Callers fall back to their ordinary project/folder name when this is not a
/// repository.
pub fn repository_name(cwd: Option<&Path>) -> Option<String> {
    let cwd = cwd?;
    let remote = Command::new("git")
        .args(["-C", cwd.to_str()?, "remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|url| {
            url.trim()
                .trim_end_matches('/')
                .rsplit(['/', ':'])
                .next()
                .map(|name| name.trim_end_matches(".git").to_string())
                .filter(|name| !name.is_empty())
        });
    if remote.is_some() {
        return remote;
    }

    Command::new("git")
        .args(["-C", cwd.to_str()?, "rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| {
            PathBuf::from(String::from_utf8(output.stdout).ok()?.trim())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_stems_are_recognised_and_others_are_not() {
        assert!(is_uuid("0199c4a1-2b3d-7e4f-8a9b-0c1d2e3f4a5b"));
        assert!(!is_uuid("memory"));
        assert!(!is_uuid("0199c4a1-2b3d-7e4f-8a9b-0c1d2e3f4a5"));
        assert!(!is_uuid("0199c4a1_2b3d_7e4f_8a9b_0c1d2e3f4a5b"));
        assert!(!is_uuid("zzzzzzzz-2b3d-7e4f-8a9b-0c1d2e3f4a5b"));
    }

    #[test]
    fn titles_collapse_whitespace_and_clip() {
        assert_eq!(tidy_title("  hello \n  world  ", 40), "hello world");
        assert_eq!(tidy_title("abcdefghij", 5), "abcde…");
        assert_eq!(tidy_title("", 5), "");
    }

    #[test]
    fn epoch_conversion_is_milliseconds() {
        let t = UNIX_EPOCH + std::time::Duration::from_millis(1_757_000_000_123);
        assert_eq!(to_ms(t), 1_757_000_000_123);
    }
}
