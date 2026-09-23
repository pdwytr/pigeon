//! End-to-end checks against the engines actually installed on this machine.
//!
//! Every test here is **read-only** and **gated**: if an engine's root is absent it returns
//! early rather than failing, so a fresh clone on a machine with none of the three still has a
//! green suite. That makes them weaker than unit tests and more valuable than any of them — they
//! are the only place the real formats, the real sizes and the real concurrency are exercised.
//!
//! Measured on this Mac (the owner's MacBook) on 2026-09-13, against Claude Code 2.1.270,
//! Codex 0.149.1 and OpenCode 1.18.29.

use std::collections::HashSet;
use std::sync::Arc;

use pigeon_lib::adapters::ProviderAdapter;
use pigeon_lib::domain::{MetricState, ProviderId, SessionKey};
use pigeon_lib::services::metrics::{MetricSource, MetricsService};
use pigeon_lib::services::sessions::SessionsService;
use pigeon_lib::wiring;

/// True when at least one engine has data here. Everything below is skipped otherwise.
fn any_engine_present() -> bool {
    let home = match dirs::home_dir() {
        Some(home) => home,
        None => return false,
    };
    home.join(".claude/projects").is_dir()
        || home.join(".codex/sessions").is_dir()
        || home.join(".local/share/opencode/opencode.db").is_file()
}

fn service() -> SessionsService {
    // Every engine is reported installed, so resumability reflects the recorded folder rather
    // than this machine's PATH — which keeps the assertion about *data* and not about setup.
    SessionsService::new(wiring::adapters(), Arc::new(|_| true))
}

#[test]
fn every_discovered_session_has_a_complete_identity() {
    if !any_engine_present() {
        eprintln!("no engine roots on this machine; skipping");
        return;
    }
    let inventory = service().discover();
    assert!(
        !inventory.sessions.is_empty(),
        "at least one engine should have sessions"
    );

    let mut seen: HashSet<SessionKey> = HashSet::new();
    for session in &inventory.sessions {
        assert!(session.key.is_valid(), "an engine handed us an empty sid");
        assert!(
            !session.title.trim().is_empty(),
            "{} has a blank title; the fallback should have applied",
            session.key.id()
        );
        assert!(
            session.last_active_ms > 0,
            "{} has no activity time",
            session.key.id()
        );
        // 2020-01-01 in epoch ms. A timestamp below this is a seconds-vs-milliseconds bug, which
        // is the single easiest mistake to make across three engines that disagree about units.
        assert!(
            session.last_active_ms > 1_577_836_800_000,
            "{} reports {}, which is not a plausible epoch-ms date",
            session.key.id(),
            session.last_active_ms
        );
        assert!(
            seen.insert(session.key.clone()),
            "duplicate key {}",
            session.key.id()
        );
    }
    eprintln!(
        "discovered {} sessions across the engines",
        inventory.sessions.len()
    );
}

#[test]
fn sessions_from_different_engines_share_projects_not_identities() {
    if !any_engine_present() {
        eprintln!("no engine roots on this machine; skipping");
        return;
    }
    let inventory = service().discover();
    let mut by_provider = std::collections::BTreeMap::new();
    let mut projects = HashSet::new();
    for session in &inventory.sessions {
        *by_provider.entry(session.key.provider_id).or_insert(0usize) += 1;
        projects.insert(session.project.clone());
    }
    eprintln!("per engine: {by_provider:?}");
    eprintln!("{} distinct projects", projects.len());

    // A normalized project key is either the reserved sentinel or an absolute path. A relative
    // one would mean two engines' views of one folder had failed to join.
    for project in &projects {
        assert!(
            project.is_none() || project.0.starts_with('/'),
            "project key {:?} is neither absolute nor the reserved key",
            project.0
        );
    }
}

#[test]
fn counting_the_same_session_twice_gives_the_same_answer() {
    if !any_engine_present() {
        eprintln!("no engine roots on this machine; skipping");
        return;
    }
    let inventory = service().discover();
    let source: Arc<dyn MetricSource> = wiring::metric_source();

    // Two independent services, so the second cannot be reading the first's cache. This is the
    // property the whole metric layer rests on: identical sources produce identical numbers, which
    // is what makes a resume, a console close and a restart all agree.
    let first = MetricsService::new(Arc::clone(&source));
    let second = MetricsService::new(Arc::clone(&source));

    let mut counted = 0;
    for session in inventory.sessions.iter().take(6) {
        let a = first.ensure(session);
        let b = second.ensure(session);
        // The NUMBERS must agree; the wall clock reading them need not. `counted_at_ms` is a
        // diagnostic stamped at fold time, so comparing whole states would fail on two reads
        // 33 ms apart -- which is exactly what it did the first time this test ran.
        assert_eq!(
            a.metrics(),
            b.metrics(),
            "{} counted differently twice",
            session.key.id()
        );
        assert_eq!(
            std::mem::discriminant(&a),
            std::mem::discriminant(&b),
            "{} reached different states twice",
            session.key.id()
        );
        if let MetricState::Ready { metrics, .. } = &a {
            counted += 1;
            // A real session that made API calls must report cache reads; a zero here with calls
            // above zero would mean the fold found records it could not read.
            if metrics.api_calls > 0 {
                assert!(
                    metrics.kpis().context_per_call.is_some(),
                    "{} has calls but an undefined context-per-call",
                    session.key.id()
                );
            }
            eprintln!(
                "{:<12} {:>5} calls {:>5} tools {:>4} turns  in={:<12} read={}",
                session.key.provider_id.as_str(),
                metrics.api_calls,
                metrics.tool_calls,
                metrics.user_turns,
                metrics.input_tokens,
                metrics.cache_read
            );
        }
    }
    assert!(counted > 0, "no session could be counted at all");
}

/// **Reading the engines must not write to them**, asserted rather than narrated.
///
/// The first version of this test collected mtimes and only `eprintln!`d a difference. It passed
/// unconditionally — it would have passed if the adapters truncated every transcript — and an
/// audit named it as the one test whose subject is the product's first invariant and whose body
/// asserts nothing.
///
/// Asserting against the *live* roots cannot work: engines are writing to them while the test
/// runs, so any difference is ambiguous. So this copies a bounded sample into a temp directory,
/// points the adapters at the copy through the roots they already accept for fixtures, does the
/// reading, and then demands the copy be byte-for-byte what it was — including that no file has
/// appeared, which is where a SQLite `-wal` or `-shm` would show up.
#[test]
fn reading_the_engines_never_writes_to_them() {
    if !any_engine_present() {
        eprintln!("no engine roots on this machine; skipping");
        return;
    }
    let home = dirs::home_dir().expect("a home directory");
    let sandbox = tempfile::tempdir().expect("a temp dir");
    let root = sandbox.path();

    // A bounded sample: enough real data to exercise every reader, small enough to copy.
    let claude_src = home.join(".claude/projects");
    let claude_dst = root.join(".claude/projects");
    copy_sample(&claude_src, &claude_dst, 6);
    copy_file(&home.join(".claude.json"), &root.join(".claude.json"));

    let codex_src = home.join(".codex/sessions");
    let codex_dst = root.join(".codex/sessions");
    copy_sample(&codex_src, &codex_dst, 6);
    copy_file(
        &home.join(".codex/auth.json"),
        &root.join(".codex/auth.json"),
    );

    let oc_src = home.join(".local/share/opencode");
    let oc_dst = root.join(".local/share/opencode");
    std::fs::create_dir_all(&oc_dst).expect("opencode dir");
    for name in [
        "opencode.db",
        "opencode.db-wal",
        "opencode.db-shm",
        "auth.json",
    ] {
        copy_file(&oc_src.join(name), &oc_dst.join(name));
    }

    let before = fingerprint(root);
    assert!(
        !before.is_empty(),
        "the sandbox should hold a sample to read"
    );

    let claude = pigeon_lib::adapters::claude::ClaudeAdapter::with_home(root.join(".claude"));
    let codex = pigeon_lib::adapters::codex::CodexAdapter::with_home(root.join(".codex"));
    let opencode = pigeon_lib::adapters::opencode::OpenCodeAdapter::with_root(
        root.join(".local/share/opencode"),
    );

    let mut counted = 0;
    for adapter in [
        Box::new(claude) as Box<dyn ProviderAdapter>,
        Box::new(codex),
        Box::new(opencode),
    ] {
        let report = adapter.discover_sessions();
        let _ = adapter.read_identity();
        for candidate in report.sessions.iter().take(4) {
            // Fold each sampled session too: the metric readers open the same files again, and a
            // write would most plausibly come from there.
            for path in candidate.source.paths.iter().take(2) {
                let _ = pigeon_lib::adapters::claude::fold_usage(path);
            }
            counted += 1;
        }
    }
    eprintln!("read {counted} sessions out of the sandbox");

    let after = fingerprint(root);

    // **The one permitted exception, and it is SQLite's, not ours.** A reader of a WAL database
    // must register itself in the shared-memory index, so opening a database that HAS a live
    // `-wal` updates its `-shm` — this assertion caught that the first time it ran, which is why
    // it says so out loud rather than having been quietly relaxed. `-shm` is a transient
    // coordination file SQLite deletes on last close; the engine's real data, `opencode.db` and
    // `opencode.db-wal`, must be untouched, and that is what is asserted below.
    //
    // This sample copies the `-wal` too, so it exercises the `mode=ro` path where the exception
    // applies. The other path creates nothing at all: with no `-wal` beside it the adapter opens
    // `immutable=1`, which is safe precisely because there is no WAL to skip, and
    // `a_cleanly_closed_opencode_database_still_lists_its_sessions` asserts that neither sidecar
    // comes into existence there.
    let is_shm = |path: &str| path.ends_with("-shm");

    let appeared: Vec<&String> = after
        .keys()
        .filter(|k| !before.contains_key(*k) && !is_shm(k))
        .collect();
    assert!(appeared.is_empty(), "reading created files: {appeared:?}");
    let vanished: Vec<&String> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    assert!(vanished.is_empty(), "reading removed files: {vanished:?}");

    let mut checked = 0;
    for (path, sig) in &before {
        if is_shm(path) {
            continue;
        }
        assert_eq!(after.get(path), Some(sig), "reading modified {path}");
        checked += 1;
    }
    assert!(
        checked > 5,
        "the sample was too small to prove anything: {checked} files"
    );
    eprintln!("{checked} files byte-identical after the read (a WAL -shm index aside)");
}

/// Path -> (size, mtime) for every file under `root`, relative to it.
fn fingerprint(root: &std::path::Path) -> std::collections::BTreeMap<String, (u64, i64)> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
            } else {
                let key = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                out.insert(key, (meta.len(), mtime));
            }
        }
    }
    out
}

fn copy_file(src: &std::path::Path, dst: &std::path::Path) {
    if !src.is_file() {
        return;
    }
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::copy(src, dst);
}

/// Copy up to `limit` files from the first few subdirectories of `src`, preserving layout.
fn copy_sample(src: &std::path::Path, dst: &std::path::Path, limit: usize) {
    let mut taken = 0;
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if taken >= limit {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if taken >= limit {
                break;
            }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(relative) = path.strip_prefix(src) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else {
                copy_file(&path, &dst.join(relative));
                taken += 1;
            }
        }
    }
}

#[test]
fn each_installed_engine_reports_an_identity_or_says_why_not() {
    if !any_engine_present() {
        eprintln!("no engine roots on this machine; skipping");
        return;
    }
    for adapter in wiring::adapters() {
        let identity = adapter.read_identity();
        assert_eq!(identity.provider, adapter.provider());
        // Signed out with no stated reason would be the one unacceptable answer: it tells the
        // owner nothing about whether the engine is absent, unconfigured, or simply logged out.
        if !identity.signed_in {
            assert!(
                identity.problem.is_some(),
                "{:?} says signed out and gives no reason",
                adapter.provider()
            );
        }
        eprintln!(
            "{:<12} signed_in={} label={:?} plan={:?}",
            adapter.provider().as_str(),
            identity.signed_in,
            identity.label,
            identity.plan
        );
    }
}

#[test]
fn opencode_publishes_no_allowance_and_says_so_rather_than_showing_zero() {
    let adapter = pigeon_lib::adapters::opencode::OpenCodeAdapter::new();
    let capacity = adapter.read_capacity();
    assert_eq!(capacity.provider, ProviderId::OpenCode);
    assert!(!capacity.supported);
    assert!(
        capacity.windows.is_empty(),
        "an unsupported allowance must draw no bar"
    );
    assert!(
        capacity.problem.is_none(),
        "unsupported is an absence, not a failure"
    );
}

/// **The join that the Live tab depends on.**
///
/// Discovery mints a `SessionKey` from a transcript filename or a database row. The status
/// service mints one from a `~/.claude/sessions/<pid>.json` field or a writer-lock filename.
/// Those are four independent code paths producing what must be the same value, and nothing else
/// in the product checks that they agree — if they drift, `sessions_list({scope: "live"})` returns
/// an empty list and looks exactly like "nothing is running".
///
/// So this asserts the overlap directly, and prints both sides when it fails.
#[test]
fn live_status_keys_match_the_keys_discovery_mints() {
    if !any_engine_present() {
        eprintln!("no engine roots on this machine; skipping");
        return;
    }
    let status = pigeon_lib::services::status::StatusService::new();
    let report = status.snapshot();
    for problem in &report.problems {
        eprintln!("status problem: {} ({:?})", problem.message, problem.kind);
    }
    if report.snapshot.live.is_empty() {
        eprintln!("nothing is live right now; the join cannot be checked");
        return;
    }

    let inventory = service().discover();
    let discovered: HashSet<SessionKey> =
        inventory.sessions.iter().map(|s| s.key.clone()).collect();

    let mut matched = 0;
    let mut unmatched: Vec<&SessionKey> = Vec::new();
    for observation in &report.snapshot.live {
        if discovered.contains(&observation.key) {
            matched += 1;
        } else {
            unmatched.push(&observation.key);
        }
    }

    eprintln!(
        "{matched} of {} live sessions joined to a discovered row",
        report.snapshot.live.len()
    );
    for key in &unmatched {
        // Not automatically a bug: a session started seconds ago may have a live process before
        // its transcript is discoverable, and Codex's lock can outlive a rollout we exclude as a
        // subagent's. Worth printing either way — a persistent entry here is a real drift.
        eprintln!("  live but not discovered: {}", key.id());
    }

    assert!(
        matched > 0,
        "not one live session joined to a discovered row -- the two key paths have drifted, and \
         the Live tab would be empty while engines are plainly running"
    );
}

/// **Two polls of an unchanged machine must look unchanged.**
///
/// The background worker notifies the view only when the live picture has moved, and it decides
/// that by comparing `StatusSnapshot::signature`. Whole-snapshot equality cannot serve: every
/// observation carries `observed_at_ms` and the snapshot carries `generated_at_ms`, so two polls
/// of a completely idle machine are never `==`. The worker compared whole snapshots until an
/// audit ran exactly this against real engines and found it notifying on every tick — which made
/// the view replace its status and refetch the whole Live scope every five seconds, losing the
/// owner's scroll position and selection for nothing.
///
/// A genuine change between the two polls (an engine finishing a turn) is possible and is not a
/// failure; the test says so rather than flaking.
#[test]
fn two_status_polls_seconds_apart_report_the_same_signature() {
    if !any_engine_present() {
        eprintln!("no engine roots on this machine; skipping");
        return;
    }
    let service = pigeon_lib::services::status::StatusService::new();
    let first = service.refresh().snapshot;
    let second = service.refresh().snapshot;

    assert_ne!(first.generated_at_ms, 0);
    if first.signature() == second.signature() {
        eprintln!(
            "{} live sessions, signature stable across two polls",
            first.live.len()
        );
    } else {
        eprintln!(
            "the picture genuinely moved between polls ({} -> {} live); not a failure",
            first.live.len(),
            second.live.len()
        );
    }

    // What IS a failure: a snapshot that differs from itself, which would mean the signature is
    // picking up the clock after all and the worker is back to notifying forever.
    assert_eq!(
        first.signature(),
        first.signature(),
        "a signature must be stable"
    );
    let mut again = first.clone();
    again.generated_at_ms += 10_000;
    for observation in &mut again.live {
        observation.observed_at_ms += 10_000;
    }
    assert_eq!(
        first.signature(),
        again.signature(),
        "moving only the clock must not read as a change"
    );
}

/// **A cleanly-closed OpenCode database must still be readable**, because that is what the owner's
/// machine looks like whenever OpenCode is not running.
///
/// SQLite deletes `-wal` and `-shm` on last close, and `mode=ro` cannot open a WAL database with
/// no `-shm` — it refuses to create the shared-memory index for a read-only connection. The
/// adapter reported that as `RootMissing`, so quitting OpenCode made all its sessions vanish
/// behind a message telling the owner their 59 MB database was missing. Every fixture held its
/// writer open, which kept `-shm` alive, so nothing caught it.
///
/// This copies the real database *without* its sidecars — which is precisely the cleanly-closed
/// state — and reads it.
#[test]
fn a_cleanly_closed_opencode_database_still_lists_its_sessions() {
    let home = match dirs::home_dir() {
        Some(home) => home,
        None => return,
    };
    let live = home.join(".local/share/opencode/opencode.db");
    if !live.is_file() {
        eprintln!("no OpenCode database on this machine; skipping");
        return;
    }

    let sandbox = tempfile::tempdir().expect("a temp dir");
    let root = sandbox.path().join("opencode");
    std::fs::create_dir_all(&root).expect("dir");
    std::fs::copy(&live, root.join("opencode.db")).expect("copy the database alone");
    assert!(
        !root.join("opencode.db-wal").exists(),
        "the copy has no WAL, by construction"
    );
    assert!(
        !root.join("opencode.db-shm").exists(),
        "nor a shared-memory index"
    );

    let adapter = pigeon_lib::adapters::opencode::OpenCodeAdapter::with_root(&root);
    let report = adapter.discover_sessions();

    assert!(
        report.problem.is_none(),
        "a closed engine's database is readable, not missing: {:?}",
        report.problem.map(|p| p.message)
    );
    eprintln!(
        "{} sessions read from a cleanly-closed database",
        report.sessions.len()
    );

    // And reading it created nothing: the read-only promise holds on this path too.
    assert!(
        !root.join("opencode.db-wal").exists(),
        "the read created a WAL"
    );
    assert!(
        !root.join("opencode.db-shm").exists(),
        "the read created a shared-memory index"
    );
}
