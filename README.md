# pigeon

A one-view console for the coding agents already running on your machine.

Pigeon reads what **Claude Code**, **Codex** and **OpenCode** write to disk, shows every session
in one list with what it cost and whether it is waiting on you, and resumes any of them in a
terminal inside the window.

It is local-only and read-only. It never mutates, moves or locks a file an engine owns, it never
bundles an engine CLI, and no credential reaches a log, an error string or the WebView.

## What it shows

**Projects first.** A project is a working directory. One folder's card shows every engine that
worked in it, because that is how the work is actually organized.

**Six counters per session.**

**No invented dollar cost.** Claude and Codex are subscription logins; a per-token price would be
fiction. Where an engine states its own figure — OpenCode does — it is shown labeled as the
engine's number.

**Live status** — running, needs you, finished, unknown — read from the host, not guessed. A
session with no live process shows no badge at all rather than a plausible-looking default.

## Run it

### Download — no toolchain needed

[**Pigeon 0.2.1 for macOS**](https://github.com/pdwytr/pigeon/releases/download/v0.2.1/Pigeon_0.2.1_aarch64.dmg)
· Apple silicon.

Open the `.dmg` and drag **Pigeon** into Applications. The first launch needs right-click →
**Open**: the build is ad-hoc signed and not notarized, so Gatekeeper warns once.

You still need whichever engines you use installed and on your `PATH`. Pigeon reads them; it does
not ship them.

### From source

Needs Rust stable and Node 20+, plus the engines on your `PATH`.

```bash
npm install
npm run tauri dev
```

**`./src-tauri/target/debug/feather` does not work on its own.** A debug binary built by plain
`cargo build` still points at `build.devUrl` — `http://localhost:1430` — so with no Vite server
running the WebView loads nothing, the window comes up blank, and not one command ever reaches
Rust. The host is fine; there is simply no page. That looks exactly like a broken app and is not
one, so: `npm run tauri dev` for development, `npm run tauri build` for a bundle with the assets
embedded. Use `cargo build` only to check that the host compiles.

## Checks

```bash
npm test               # vitest
npm run typecheck      # BOTH tsconfig projects, not just src/
npx biome check src

cd src-tauri
cargo test
cargo clippy --all-targets -- -D warnings
```

`npm run typecheck` checks both TypeScript projects on purpose. A bare `tsc --noEmit` type-checks
neither `vite.config.ts` nor anything else outside `include` — a trap Demo Studio fell into and
documented.

## Where things are

```
src/                      React view. src/bindings.ts is the wire contract.
src-tauri/src/
  domain/                 provider-neutral vocabulary: keys, metrics, status, projects
  adapters/               one file per engine; nothing above this line knows their formats
  services/               discovery, metrics, status, accounts, projects, consoles
  api/                    Tauri commands, DTOs, events, the typed error vocabulary
docs/contracts/           the specification this was built from
docs/UI/                  the functional mockups
```

## The rules this codebase keeps

Inherited from Demo Studio, where each was learned the expensive way.

1. **Read-only on sources.** Snapshot and report; never write back.
2. **Fail loud on unknown shapes.** Engine formats drift — twice, provably. An adapter that meets
   a record it does not recognise says so. It never renders a number derived from a guessed schema.
3. **The counting rules are frozen.** Dedupe API calls by `message.id`; within one id take the
   per-key elementwise **MAX**, because `output_tokens` is a streaming counter. Summing raw lines
   double-counts 2–6×.
4. **These numbers are owner diagnostics, never agent targets.** Handing a builder a Studio metric
   to hit is how you get Goodhart's law instead of a measurement.
5. **Hold a lock only for the work that needs it.** Resolve, lock, write, unlock. Never across a
   spawn, a PTY write, a `wait`, an emit, or a filesystem walk.
6. **A session is `(provider, sid)`.** Never `sid` alone — two engines may mint the same uuid — and
   never a prefix: UUIDv7 prefixes collide every ~65 seconds.
7. **Null, pending, unavailable and zero are four different things**, and the View renders four
   different things.

## Not in this pass

macOS signing and notarization, Windows and Linux runtime verification, multi-profile logins,
OpenCode capacity (it publishes none), filesystem watching, and any persistent store.
