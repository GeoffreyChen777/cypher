# Development

How the repository is laid out, the rules the code follows, and how to run the
same checks CI runs. [ARCHITECTURE.md](../../ARCHITECTURE.md) explains how the
product works; this guide is about working on it.

## Repository map

| Path | What it is |
| --- | --- |
| `Cargo.toml`, `rust-toolchain.toml`, `rustfmt.toml` | Rust workspace, pinned toolchain (with rustfmt and clippy), formatter settings |
| `crates/env` | `cypher-env`: every `CYPHER_*` environment variable, data directories, IPC socket paths |
| `crates/proto` | `cypher-proto`: wire types and the pure view derivations both frontends share |
| `crates/syntax` | `cypher-syntax`: tree-sitter syntax highlighting |
| `crates/doc` | `cypher-doc`: Loro session-doc and workspace-registry schemas, mirror layer, parts fold |
| `crates/update` | `cypher-update`: release checking and self-update for the engine, CLI and UI |
| `crates/net` | `cypher-net`: WebSocket dialing (happy eyeballs, proxies) and wake broadcasts |
| `crates/sync` | `cypher-sync`: Edge room clients (chat2, registry), presence, the SQLite docs store |
| `crates/harness` | `cypher-harness`: the `Harness` trait, Pi over its RPC protocol, the mock harness |
| `crates/rpc` | `cypher-rpc`: the typed RPC protocol, Unix IPC transport and device-room relay |
| `crates/engine` | `cypher-engine`: the engine — sessions, doc host, repos, terminals, auth, RPC service |
| `crates/ui` | `cypher-ui`: the gpui desktop app |
| `apps/cypher` | the `cypher` binary: headed app, `headless` engine and the CLI |
| `apps/edge` | the Cloudflare Worker and Durable Objects (TypeScript; [README](../../apps/edge/README.md)) |
| `apps/ios` | the SwiftUI iPhone client ([apps/ios/README.md](../../apps/ios/README.md)) |
| `apps/landing`, `apps/www-redirect` | the landing page and the www redirect Workers |
| `pi-runtime/` | the curated Pi runtime bundle: extensions, patches, release metadata ([README](../../pi-runtime/README.md)) |
| `packaging/` | packaging inputs: app icon, macOS `Info.plist` template and dmg art ([README](../../packaging/README.md)) |
| `scripts/` | `dev-*` (local dev loop), `package-*` (release packaging), `check.sh` (all checks) |
| `scripts/ci/` | release tooling and CI helpers (`release.py`, actionlint, ShellCheck, crate layers, Xcode selection) |
| `scripts/tests/` | tests of the scripts and installer, cross-language vector runners, the doc link checker |
| `scripts/ops/` | Cloudflare usage and billing probes ([Cloudflare billing](../operations/cloudflare-billing.md)) |
| `docs/` | documentation, indexed in [docs/README.md](../README.md) |
| `.github/workflows/` | CI, deploy and release workflows ([CI/CD operations](../operations/ci-cd.md)) |
| `.agents/skills/` | runbooks for coding agents (the dev app procedure) |

## Crate layering

Each crate sits in one layer and depends only on crates in lower layers, in
`[dependencies]` and `[dev-dependencies]` alike. `scripts/ci/check-crate-layers.py`
holds the table and fails CI when a manifest breaks it; a new crate must be
added there.

| Layer | Crates |
| --- | --- |
| foundation | `cypher-env`, `cypher-proto`, `cypher-syntax` |
| model and transport | `cypher-doc`, `cypher-update`, `cypher-net` |
| clients | `cypher-sync`, `cypher-harness` |
| relay | `cypher-rpc` |
| engine | `cypher-engine` |
| desktop UI | `cypher-ui` |
| application | `cypher` |

Test-only use of a crate belongs in `[dev-dependencies]`.

## Checks

`scripts/check.sh` is the one entry point; every CI step runs one of its
stages, so a stage that passes locally passes in CI.

```sh
bash scripts/check.sh fmt        # cargo fmt --check
bash scripts/check.sh lint       # crate layers, Python byte-compile, bash -n, ShellCheck,
                                 # node --check, clippy -D warnings (headless crates)
bash scripts/check.sh rust       # headless crate tests, headless build, CLI tests
bash scripts/check.sh edge       # apps/edge typecheck, unit and workerd tests
bash scripts/check.sh scripts    # script/release/installer tests, doc links
bash scripts/check.sh runtime    # Pi runtime suites that need no staged runtime
bash scripts/check.sh workflows  # actionlint + workflow policy (downloads actionlint)
bash scripts/check.sh macos      # macOS: icon, workspace clippy, cypher-ui tests,
                                 # Rust/Swift preview vectors
bash scripts/check.sh ios        # iOS unit tests on a simulator (Xcode 27)
bash scripts/check.sh all        # everything except ios; macos only on a Mac
```

`lint` and `workflows` download pinned ShellCheck and actionlint binaries on
first use. `CARGO=…`, `PYTHON=…` and `NODE=…` substitute the tools, for example
a wrapper that serializes heavy builds.

Tests per platform:

- **Rust**: inline `#[cfg(test)] mod tests`, `foo/tests.rs` for larger suites,
  integration tests in `<crate>/tests/` sharing `tests/common/`. Some suites
  need a feature: `cypher-sync --features mock-server` (registry transport),
  `cypher-engine --features development` and `cypher --features development`.
  `cargo test --workspace` runs every default-feature suite on a Mac.
- **Edge**: `npm --prefix apps/edge test` runs the vitest unit tests beside each
  module (`src/**/*.test.ts`, including the golden route table in
  `src/index.test.ts`) and the workerd tests (`test/workerd/`) against real
  Durable Object SQLite; see [apps/edge/README.md](../../apps/edge/README.md).
- **Pi runtime**: `npm --prefix pi-runtime test` runs the `*.test.mjs` suites
  beside each module; the `*.staged.test.mjs` suites need a staged runtime and run
  inside `scripts/package-pi-runtime.sh` (see
  [pi-runtime/README.md](../../pi-runtime/README.md#tests)).
- **iOS**: `bash scripts/check.sh ios`, or Xcode; see
  [apps/ios/README.md](../../apps/ios/README.md).
- **Cross-language vectors**: the registry merge vectors (Rust, TS, Swift) and the
  preview vectors in `crates/sync/tests/fixtures` (Rust and Swift) must change together.
- **Scripts**: `scripts/tests/`; `test-linux-cli.py` also runs under Python 3.8 in
  an Ubuntu 20.04 container, so it and `apps/edge/src/install.sh` stay 3.8- and
  POSIX-sh-compatible.

## Running the app

- `scripts/dev-engine.sh [local|dev]` builds the `development` feature and runs a
  headless engine with the mock harness; `scripts/dev-app.sh [local|dev]` runs a
  UI attached to it over Unix IPC. Data lives in `~/.cypher-development/`;
  `CYPHER_DEV_INSTANCE=<name>` keeps a second checkout's instance separate.
- `scripts/dev-demo.sh` starts an offline engine seeded with demo chats.
- `scripts/dev-ios.sh` runs the iOS Dev bundle against a local Edge.
- `dev` mode syncs through a local Edge: [Local Edge development](local-edge.md).
- `scripts/e2e-smoke.sh` runs two headless engines against a local Edge.

The step-by-step rebuild-and-restart procedure (which processes to touch and how
to verify them) is in
[`.agents/skills/cypher-dev-app/SKILL.md`](../../.agents/skills/cypher-dev-app/SKILL.md).

## Conventions

### Rust

1. Module files: `foo.rs` + `foo/` for children; no `mod.rs` under `src/`
   (`tests/common/mod.rs` in integration tests is exempt — cargo layout).
2. Tests: inline `#[cfg(test)] mod tests {}` up to ~150 lines; larger go to
   `foo/tests.rs` declared with `#[cfg(test)] mod tests;`. Integration tests in
   `<crate>/tests/` sharing `tests/common/`.
3. Imports: leaf/pure modules import explicitly; `use super::*` only in
   continuation files that extend the parent's type (`impl Shell` split files etc.).
4. Naming: child module rendering code is `render.rs` (not `rendering.rs`);
   settings pages are `<Topic>Page`; the epoch-milliseconds helper is `now_ms()`.
5. Visibility: inside private modules use bare `pub` for crate-visible items and
   `pub(super)` for parent-only; in library crates' public modules, `pub` is API —
   keep internals `pub(crate)` or private.
6. Errors: one thiserror enum per library crate; `anyhow` only in `apps/cypher`
   (`cypher-update` is a documented exception); no new `Result<_, String>` APIs in
   the engine.
7. Logging: `tracing` with structured fields (`error = %err`); no print macros in
   library crates; no explicit `target:`.
8. Env: every `CYPHER_*` read goes through `cypher_env`.
9. Locks: `std::sync::Mutex` for short critical sections, never held across
   `.await`; poisoning ignored through one `lock()` helper per crate.
10. Comments explain current behaviour and why; no dates, incident stories,
    review-round labels, or citations of files that don't exist.

Lints live in `[workspace.lints]` in the root `Cargo.toml`; every package opts in
with `[lints] workspace = true`, and CI runs clippy with `-D warnings`. Crates
without unsafe code (`proto`, `doc`, `sync`, `update`, `syntax`) declare
`#![forbid(unsafe_code)]`. Prefer `#[expect(lint, reason = "…")]` for a new
local suppression.

### Swift (`apps/ios`)

- One primary type per file (`Type+Aspect.swift` for extension splits); the first
  line is a `//` purpose comment, and ports cite the desktop source file.
- Folders by feature (Home, Session, Workspace, Notifications, Auth, Composer,
  Transcript, Markdown) or layer (Sync, Models, Theme, Shared); dev rigs in
  `Development/`.
- Stores are `@MainActor @Observable final class`, transports are `actor`, members
  are `private` by default.
- Tests are named for behaviour; helpers live only in `TestSupport`.

### TypeScript (`apps/edge`, `pi-runtime`)

- Kebab-case file names, ESM, 2-space indent, double quotes, semicolons,
  `import type` for types.
- Unit tests beside the module and named after it (`*.test.ts`; `*.test.mjs`
  in `pi-runtime`); workerd tests in `test/workerd/`, shared test helpers in
  `test/support/`.
- No raw control characters in source; comments describe current behaviour.
- The edge layout, handler chain and its extra rules (one Durable Object class
  per file, `Response | undefined` handlers, one identifier module, strict
  indexing) are in [apps/edge/README.md](../../apps/edge/README.md).

### Other files

`.editorconfig` records the indentation: 4 spaces for Rust, Swift and Python;
2 spaces for TypeScript, JavaScript, JSON, YAML, CSS, HTML and shell; LF line
endings and a final newline everywhere. Python scripts use only the standard
library, except `scripts/dmg-background.py`, which needs Pillow.

### Commits and branches

- Subject: imperative, sentence case, no trailing period, no `type(scope):`
  prefix, at most 72 characters. The body explains why when that is not obvious.
- Pure moves (`git mv` with only path-reference updates) go in their own commits so
  history follows renames; logic changes are separate commits.
- Release commits: `Release Cypher X.Y.Z` / `Release Cypher X.Y.Z for macOS`;
  runtime: `Runtime A.B.C.D: <summary>`.
- Tags: `cypher-linux-v*`, `cypher-macos-v*`, `cypher-ios-v*-b*`, `pi-runtime-v*`
  (checked by `scripts/ci/release.py`).
- Branches: `cypher/<topic>` for worktrees (the dev-instance tooling assumes it),
  otherwise `feat/*`, `refactor/*`; topic branches merge with a merge commit
  (`Merge branch '<name>'`).

## Before merging

- `bash scripts/check.sh all` passes (plus `ios` when iOS code changed).
- User-visible behaviour, wire formats, CLI flags, environment variables, settings
  files and release artifact names are unchanged, or the change says so.
- Docs that describe the changed code are updated, and links still resolve
  (`python3 scripts/tests/check-doc-links.py`).
- A cross-language contract (registry merge, chat2 frames, device frames, preview
  vectors) changed in every implementation and its shared vectors.
- Release notes for user-visible changes go in `docs/releases/` with the release.
