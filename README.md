# pigeon

**One window for every coding agent running on your Mac.**

Pigeon reads what **Claude Code**, **Codex** and **OpenCode** write to disk and lists every session
in one place: which project it belongs to, what it has used, and whether it is waiting on you. A
small always-on-top hover window keeps the live count in view while you work; the full console
resumes any session in an embedded terminal.

It is local-only and read-only. It never mutates, moves or locks a file an engine owns, never
bundles an engine CLI, and no credential reaches a log, an error string or the WebView.

## Screenshots

<!-- Placeholder: add a screenshot or GIF of the hover window and the console, e.g. docs/images/hover.png -->
_Screenshots coming soon._

## Features

- **Three engines, one list.** Claude Code, Codex and OpenCode sessions side by side.
- **Projects first.** A project is a working directory; its card shows every engine that worked there.
- **Live status** — running, delegating, needs you, finished, unknown — read from the host, never
  guessed. A session whose engine cannot be read shows no status instead of a confident `finished`.
- **Per-session counters** (tokens, API calls, tool calls, turns, duration), computed in the
  background so the list appears instantly.
- **No invented dollar cost.** Claude and Codex are subscription logins, so no per-token price is
  made up. Where an engine states its own figure (OpenCode), it is shown and labeled as the engine's.
- **Resume in place** in a terminal inside the window, and stop a session only when its process can
  be proven to belong to it.
- **Private by construction.** Nothing is sent anywhere; credentials and command lines never enter
  a payload, log or error.

## Requirements

- macOS (Apple silicon release build; Windows and Linux are not runtime-verified).
- The engines you use (`claude`, `codex`, `opencode`) installed and on your `PATH`. Pigeon reads
  them; it does not ship them.
- To build from source: Rust (stable) and Node 20+.

## Install

### Download

[**Pigeon 0.3.2 for macOS**](https://github.com/pdwytr/pigeon/releases/download/v0.3.2/Pigeon_0.3.2_aarch64.dmg)
(Apple silicon). Open the `.dmg` and drag **Pigeon** into Applications. The build is ad-hoc signed
and not notarized, so the first launch needs right-click → **Open**.

### Build from source

```bash
git clone https://github.com/pdwytr/pigeon.git
cd pigeon
npm install
npm run tauri dev      # development, with hot reload
npm run tauri build    # release bundle (.app and .dmg) with assets embedded
```

> **Do not run `src-tauri/target/debug/pigeon` directly.** A plain `cargo build` binary still points
> at the Vite dev server (`http://localhost:1430`), so with no server running the window is blank.
> Use `cargo build` only to check that the Rust host compiles.

## Usage

Launch Pigeon, then start Claude Code, Codex or OpenCode sessions as usual. They appear on their own.

- **Hover window** — a compact, always-on-top list of live sessions and how many need you.
- **Console** — projects, per-session counters, details and the embedded terminal for resuming.

Status meanings, counting rules and failure handling are documented in
[docs/how-it-works.md](docs/how-it-works.md).

## Development

```bash
npm test                 # vitest
npm run typecheck        # both tsconfig projects
npx biome check src

cd src-tauri
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

`npm run gate` runs all of the above.

```
src/                      React view. src/bindings.ts is the wire contract.
src-tauri/src/
  domain/                 provider-neutral vocabulary: keys, metrics, status, projects
  adapters/               one file per engine; nothing above this line knows their formats
  services/               discovery, metrics, status, accounts, projects, consoles
  api/                    Tauri commands, DTOs, events, the typed error vocabulary
docs/contracts/           the specification this was built from
docs/decisions/           architecture decision records
docs/planning/            working plan and task checklist
docs/ui/                  the functional mockups
```

## Not in this release

macOS signing and notarization, Windows and Linux runtime verification, multi-profile logins,
filesystem watching, and any persistent store.

## License

No license file has been added yet, so all rights are reserved by default. The choice of license
is the owner's; see the open issue.
