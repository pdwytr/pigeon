//! The PATH a spawned engine is actually found on, and the environment a PTY child needs.
//!
//! -- WHY THIS FILE EXISTS (measured on this Mac, 2026-09-13) ------------------------------------
//!
//! A macOS app launched from Finder, Dock or Spotlight does not inherit the shell's environment.
//! `launchctl getenv PATH` is unset on this machine, so the app starts with the kernel/launchd
//! default — `/usr/bin:/bin:/usr/sbin:/sbin` — and **none of the three engine binaries is there**:
//!
//!   claude    ~/.local/bin/claude        (the native installer)
//!   codex     ~/.bun/bin/codex           (bun global install)
//!   opencode  /opt/homebrew/bin/opencode (homebrew, Apple silicon prefix)
//!
//! Run the same build from a terminal and every one of them resolves, which is exactly the failure
//! mode that gets reported as "it works when I run it from the shell". So Pigeon reconstructs the
//! PATH the owner's own login shell would give it, once, and unions the well-known install
//! directories onto it — a union rather than a replacement, because the inherited PATH is the only
//! thing guaranteed to hold `/usr/bin` and friends.
//!
//! Nothing here ever returns an empty PATH: a broken `~/.zshrc`, a shell that is not on disk, or a
//! probe that hangs all land on the same fallback (inherited + the well-known set), because a
//! console that cannot find `claude` is a far better failure than one that cannot find `/bin/sh`.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// How long the login-shell probe may take before Pigeon gives up and uses the fallback.
///
/// **A timeout, not a tuning knob.** The probe runs a LOGIN shell, which sources the owner's whole
/// rc chain — nvm, pyenv, conda, direnv, a corporate MDM script. Any one of those can block on a
/// network mount or a prompt. This probe sits on the path to opening a console, so a shell that
/// never returns would otherwise be indistinguishable from a hung app. 2 seconds is generous
/// against a warm zsh and short enough to still feel like a click.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The terminal type Pigeon claims for its children.
///
/// Studio sets none and does not need to: ConPTY hands the child a Windows console and the CLIs
/// detect it through the Win32 API rather than through `TERM`. A unix PTY carries no such signal —
/// an unset `TERM` makes the child fall back to `dumb`, which strips colour, disables the
/// alternate screen and makes an engine's TUI unusable. `xterm-256color` is what Terminal.app and
/// iTerm2 both announce, and what the View's xterm.js actually implements.
const TERM: &str = "xterm-256color";

/// 24-bit colour. xterm.js renders true colour; without this the CLIs quantise to 256.
const COLORTERM: &str = "truecolor";

/// Only used when the owner's environment names no locale at all. UTF-8 matters here: the engines
/// draw box-drawing characters and emoji, and a child that believes it is in the C locale escapes
/// them into mojibake.
const FALLBACK_LANG: &str = "en_US.UTF-8";

/// Install directories unioned onto whatever the probe returns, in the order a `which` would find
/// them. Absolute paths only; `~` is expanded against the real home, never left for a shell.
fn well_known_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir();
    let under_home = |leaf: &str| home.as_ref().map(|h| h.join(leaf));
    [
        under_home(".local/bin"), // claude, the native installer's prefix
        under_home(".bun/bin"),   // codex, installed through bun
        Some(PathBuf::from("/opt/homebrew/bin")), // opencode; the Apple-silicon brew prefix
        Some(PathBuf::from("/usr/local/bin")), // the Intel brew prefix, and `npm -g` setups
        under_home(".cargo/bin"), // cargo-installed tools alongside the engines
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The PATH Pigeon resolves engine binaries on and hands to every console child.
///
/// Cached: the probe is one login shell per process, however many consoles are opened. A PATH that
/// changes underneath a running app is not picked up, which is the same contract a terminal
/// emulator has — the owner restarts the app, exactly as they would open a new tab.
pub fn effective_path() -> OsString {
    static CACHED: OnceLock<OsString> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            compose(
                login_shell_path().as_deref(),
                std::env::var_os("PATH").as_deref(),
            )
        })
        .clone()
}

/// The environment overrides a PTY child needs, on top of the inherited environment.
///
/// Applied over the inherited base rather than replacing it (`CommandBuilder::from_argv` seeds
/// itself from `std::env::vars_os`), so the child keeps the owner's proxy settings, their config
/// paths and their credentials — a console that lost those would be signed out of the engine it
/// had just resumed.
pub fn child_env() -> Vec<(String, String)> {
    compose_child_env(effective_path(), std::env::var_os("LANG"))
}

/// [`child_env`]'s pure half, so the LANG rule is a test rather than a claim about process state.
fn compose_child_env(path: OsString, lang: Option<OsString>) -> Vec<(String, String)> {
    let mut env = vec![
        ("PATH".to_string(), path.to_string_lossy().into_owned()),
        ("TERM".to_string(), TERM.to_string()),
        ("COLORTERM".to_string(), COLORTERM.to_string()),
    ];
    // **Never clobbered.** An owner who runs in `en_GB.UTF-8` or `ja_JP.UTF-8` has said what they
    // want, and overwriting it would change how their CLI formats dates and sorts. A blank value
    // counts as unset — `LANG=` is what a stripped launchd environment looks like, not a choice.
    if lang.map(|v| v.is_empty()).unwrap_or(true) {
        env.push(("LANG".to_string(), FALLBACK_LANG.to_string()));
    }
    env
}

/// Union the probe's PATH, the inherited PATH and the well-known install dirs, in that order.
///
/// **Order is the whole contract.** The login shell's own order decides which `claude` the owner's
/// terminal runs, and Pigeon has to resolve the same one — a different answer would launch a
/// different CLI than `claude --version` reports in their shell. The inherited PATH goes next
/// because it is the only entry guaranteed to hold `/usr/bin`, and the well-known dirs go last
/// because they are a safety net, not a preference.
///
/// Deduped on first occurrence, so a directory named by both the shell and the defaults keeps the
/// shell's position.
fn compose(probe: Option<&str>, inherited: Option<&OsStr>) -> OsString {
    let mut seen: HashSet<OsString> = HashSet::new();
    let mut ordered: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if dir.as_os_str().is_empty() {
            return;
        }
        if seen.insert(dir.as_os_str().to_os_string()) {
            ordered.push(dir);
        }
    };
    if let Some(probe) = probe {
        for dir in std::env::split_paths(probe) {
            push(dir);
        }
    }
    if let Some(inherited) = inherited {
        for dir in std::env::split_paths(inherited) {
            push(dir);
        }
    }
    for dir in well_known_dirs() {
        push(dir);
    }
    // `join_paths` only fails on a component containing the separator itself, which cannot come
    // out of `split_paths` and is not true of the literals above. The fallback is the inherited
    // value rather than nothing, because an empty PATH is the one answer this module must never
    // give.
    std::env::join_paths(&ordered)
        .unwrap_or_else(|_| inherited.map(OsStr::to_os_string).unwrap_or_default())
}

/// Ask the owner's login shell what PATH it would give them. `None` when there is no usable
/// answer.
///
/// `$SHELL -lc 'printf %s "$PATH"'` and nothing else: `-l` is what sources `.zprofile` /
/// `.profile` (where Homebrew's `shellenv` and most installers write their PATH lines), `printf
/// %s` emits no trailing newline, and the quoting keeps a PATH with spaces in it in one word.
/// stdin is `/dev/null` so an rc file that reads from the terminal gets EOF instead of hanging the
/// app.
#[cfg(unix)]
fn login_shell_path() -> Option<String> {
    static PROBED: OnceLock<Option<String>> = OnceLock::new();
    PROBED.get_or_init(probe_login_shell).clone()
}

/// No login-shell concept, and no measured failure to fix: a Windows process inherits the user's
/// PATH from the registry however it was started.
#[cfg(not(unix))]
fn login_shell_path() -> Option<String> {
    None
}

#[cfg(unix)]
fn probe_login_shell() -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"));
    let mut child = Command::new(&shell)
        .args(["-lc", "printf %s \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;

    // Read on a thread and wait on a channel, because `wait_with_output` consumes the child and
    // would leave nothing to kill when the rc chain blocks. The child handle stays here so the
    // timeout has teeth: kill, then reap, so a hung shell cannot outlive the probe as a zombie.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    let answer = rx.recv_timeout(PROBE_TIMEOUT);
    if answer.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();

    answer
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn entries(path: &OsStr) -> Vec<PathBuf> {
        std::env::split_paths(path).collect()
    }

    fn contains(path: &OsStr, dir: &Path) -> bool {
        entries(path).iter().any(|e| e == dir)
    }

    #[test]
    fn the_five_well_known_install_dirs_are_present_even_when_the_probe_answers_nothing() {
        let composed = compose(None, Some(OsStr::new("/usr/bin:/bin")));
        for dir in well_known_dirs() {
            assert!(
                contains(&composed, &dir),
                "{} is missing from {composed:?}",
                dir.display()
            );
        }
        assert_eq!(
            well_known_dirs().len(),
            5,
            "the well-known set is the five measured prefixes"
        );
        // And the inherited entries survive: a PATH without /usr/bin cannot even run /bin/sh.
        assert!(contains(&composed, &PathBuf::from("/usr/bin")));
        assert!(contains(&composed, &PathBuf::from("/bin")));
    }

    #[test]
    fn duplicate_directories_collapse_to_one_and_keep_their_first_position() {
        let composed = compose(Some("/a:/b:/a:/c"), Some(OsStr::new("/b:/d")));
        let listed = entries(&composed);
        let head: Vec<PathBuf> = listed.iter().take(4).cloned().collect();
        assert_eq!(
            head,
            ["/a", "/b", "/c", "/d"].map(PathBuf::from).to_vec(),
            "the login shell's order decides which engine binary wins"
        );
        assert_eq!(
            listed
                .iter()
                .filter(|e| e.as_path() == Path::new("/a"))
                .count(),
            1
        );
        assert_eq!(
            listed
                .iter()
                .filter(|e| e.as_path() == Path::new("/b"))
                .count(),
            1
        );
        // An empty entry (a trailing `:` in someone's .zshrc) is dropped rather than carried as
        // "", which `split_paths` would otherwise hand a resolver as a relative directory.
        let gappy = compose(Some("/a::/b:"), None);
        assert!(!entries(&gappy).iter().any(|e| e.as_os_str().is_empty()));
    }

    #[test]
    fn child_env_sets_term_and_colorterm_and_leaves_an_existing_lang_alone() {
        let lookup = |env: &[(String, String)], key: &str| {
            env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        };
        let with_lang = compose_child_env(
            OsString::from("/usr/bin"),
            Some(OsString::from("ja_JP.UTF-8")),
        );
        assert_eq!(lookup(&with_lang, "TERM").as_deref(), Some(TERM));
        assert_eq!(lookup(&with_lang, "COLORTERM").as_deref(), Some(COLORTERM));
        assert_eq!(lookup(&with_lang, "PATH").as_deref(), Some("/usr/bin"));
        assert_eq!(
            lookup(&with_lang, "LANG"),
            None,
            "an owner's own locale is never overridden"
        );

        // Unset, and the blank that a stripped launchd environment produces, both get the
        // fallback.
        for absent in [None, Some(OsString::new())] {
            let env = compose_child_env(OsString::from("/usr/bin"), absent);
            assert_eq!(lookup(&env, "LANG").as_deref(), Some(FALLBACK_LANG));
        }

        // The real one, against this machine's environment: those two variables are the whole
        // reason a unix PTY child is not a dumb terminal.
        let real = child_env();
        assert_eq!(lookup(&real, "TERM").as_deref(), Some(TERM));
        assert_eq!(lookup(&real, "COLORTERM").as_deref(), Some(COLORTERM));
        assert!(lookup(&real, "PATH")
            .map(|p| !p.is_empty())
            .unwrap_or(false));
    }

    #[test]
    fn the_effective_path_is_never_empty_however_the_probe_goes() {
        let real = effective_path();
        assert!(
            !real.is_empty(),
            "an empty PATH would lose /bin/sh, not just the engines"
        );
        assert!(!entries(&real).is_empty());

        // Every degenerate input: no probe, no inherited PATH, an empty probe, a probe of nothing
        // but separators. None of them may produce an empty answer.
        for (probe, inherited) in [
            (None, None),
            (Some(""), None),
            (Some("::"), None),
            (None, Some(OsStr::new(""))),
        ] {
            let composed = compose(probe, inherited);
            assert!(
                !composed.is_empty(),
                "compose({probe:?}, {inherited:?}) gave an empty PATH"
            );
            assert!(!entries(&composed).is_empty());
        }

        // Cached: the second call is the same answer, which is what makes "one login shell per
        // process" true rather than aspirational.
        assert_eq!(real, effective_path());
    }

    /// The probe is the one part of this file that runs another program, so it gets its own
    /// assertion on this machine: a real login shell, a real PATH, inside the timeout.
    #[cfg(unix)]
    #[test]
    fn the_login_shell_probe_answers_within_its_timeout_on_this_machine() {
        let started = std::time::Instant::now();
        let answer = login_shell_path();
        let elapsed = started.elapsed();
        assert!(
            elapsed < PROBE_TIMEOUT + Duration::from_millis(500),
            "the probe overran its own timeout: {elapsed:?}"
        );
        // A machine with no usable login shell is a legitimate skip; a shell that answers at all
        // must answer with something shaped like a PATH.
        if let Some(path) = answer {
            assert!(!path.trim().is_empty());
            assert!(path.contains('/'), "a PATH is made of paths: {path:?}");
        }
    }
}
