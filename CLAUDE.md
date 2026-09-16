# CLAUDE.md — pigeon

Operating rules for any Claude session in this repo. Read this first.

## What this is

A one-view console for the coding agents already running on this machine. It reads what **Claude
Code**, **Codex** and **OpenCode** write to disk, shows every session with what it cost and whether
it is waiting on you, and resumes any of them in a terminal inside the window. Tauri v2 host, React
view, no Python, **no store**.

It **copies** from `../demo-studio` and never imports it. The counting rules, the PTY runtime
and the locking discipline came from there; the two products must agree on every number.

## Read before working

`docs/contracts/` is the specification — `urds.md`, `frds.md`, `apis.md` (the IPC contract),
`data.md`, `objects.md`, `components.md`. `docs/decisions/` records the rulings the build forced.
`docs/UI/` holds the functional mockups the view is measured against.

## Invariants — do not break

1. **Read-only on the engines' files.** Never write, move, lock, truncate or *create* anything
   under `~/.claude`, `~/.codex` or `~/.local/share/opencode`. The one permitted exception is
   SQLite's own `-shm` index, which a WAL reader must register itself in;
   `tests/real_machine.rs` asserts everything else byte-for-byte against a copied sample.
2. **Fail loud on unknown shapes.** Engine formats drift — provably, twice in the sibling product.
   An adapter that meets a record it does not recognise says so. It never renders a number derived
   from a guessed schema, and it never substitutes a default for a field it could not read.
3. **The counting rules are frozen.** Dedupe API calls by `message.id`; within one id take the
   per-key elementwise **MAX**, because `output_tokens` is a streaming counter. Summing raw lines
   double-counts 2–6×. Changing a definition needs a decision record.
4. **These numbers are owner diagnostics, never agent targets.**
5. **A session is `(provider, sid)`.** Never `sid` alone — two engines may mint the same uuid — and
   never a prefix: UUIDv7 prefixes collide every ~65 seconds.
6. **Null, pending, unavailable and zero are four different things**, and the view renders four
   different things. A zero denominator is *undefined*, not zero.
7. **No credential in any payload, log, error or `Debug` output.** `Secret::expose()` appears once
   outside tests; grep for it to audit every use.
8. **An empty answer must never be able to mean "we could not look."** An empty Live tab that looks
   like an idle machine is the one way this product can lie. Every scope carries its problems, and
   a session whose engine could not be read has *no* status rather than "finished".
9. **Hold a lock only for the work that needs it** — resolve, lock, write, unlock. Never across a
   spawn, a PTY write, a `wait`, an emit, a filesystem walk or an HTTP call. Every mutex is a
   non-reentrant `std::sync::Mutex`, so a second acquisition on one thread deadlocks permanently
   rather than warning.
10. **Never guess a process attribution.** A cwd match is not proof; two sessions can run in one
    folder. Ambiguity means `ProcessAmbiguous` and **stop nothing**. No evidence string may contain
    a command line — argv can hold a prompt.

## Things that have already bitten, in this repo

- **`spawn_blocking` is not "off the runtime".** A `spawn_blocking` worker is still in a runtime
  context — that is what makes `Handle::current()` work there — so `Runtime::new().block_on()`
  still panics. Adapter calls that build a runtime go on a plain `std::thread`.
- **Tauri v2 resolves each command parameter by its own camelCased name at the top level** of the
  invoke payload. An `{ args: {...} }` envelope hides every one of them, and only commands whose
  parameters are all `Option` survive it. `src/api/client.test.ts` pins this.
- **Comparing whole snapshots compares their clocks.** Anything with a timestamp is never equal to
  itself a second later, so change-detection needs a signature with the clock left out.
- **`mode=ro` cannot open a WAL database with no `-shm`**, which is the normal state of a cleanly
  closed one. "Cannot open" is not "not there".
- **A bare `cargo build` binary points at the dev server.** `./src-tauri/target/debug/feather`
  loads `build.devUrl`, so without Vite running the window is blank and no command reaches Rust —
  which looks precisely like a broken app. Verify behaviour through `npm run tauri dev` or a
  bundle; use `cargo build` only to check that the host compiles.
- **Engine status degrades per provider.** One unreadable root leaves that engine's sessions out
  of the live set for a reason that says nothing about whether they are running.

## Conventions

- **Conventional commits**; default branch `master`. Commit per completed unit of work.
- `npm run gate` runs everything: both tsconfig projects, biome, vitest, rustfmt, clippy at
  `-D warnings`, and the Rust suite. A bare `tsc --noEmit` checks neither config file — check both
  projects explicitly. `--all-targets` is load-bearing on check and clippy, or `#[cfg(test)]` code
  is never type-checked.
- Comments explain **why**, with the measurement that justifies them. Test names are full sentences.
- Dev server is **1430**: demo-studio reserves 1420–1422 and `strictPort` makes an overlap a
  hard failure.

## Working with parallel agents in this tree

**Give each agent exactly one file, and say so.** The lanes that built this each owned one module,
and the one real collision came from the coordinator, not the lanes.

**Scope every commit by pathspec.** `git add -A` while a lane is mid-edit sweeps its unfinished
work into someone else's commit — which happened here, to the status lane, and mislabelled its
history. Use `git commit -- <paths>`, and check `git diff --cached --name-only` before committing.

**Do not edit a file you have handed to an agent**, even for a one-line change. Send the agent the
change instead; it owns the file until it reports.

**Tests are the deliverable, not the code.** Every fix in this repo's audit round was required to
be proven red before it was accepted. A test that would still pass if the code were wrong is worse
than no test, because it is counted.
