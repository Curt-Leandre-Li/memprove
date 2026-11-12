# Contributing to memprove

memprove is a Rust workspace: a daemon (`memproved`), a command-line
client (`memprove`), and the crates they share. Everything a change needs
is in this repository; nothing is fetched from the network at build time.

## Building and testing

The SQLite crate's build script runs bindgen, which needs libclang. On macOS
with Homebrew LLVM:

```bash
export LIBCLANG_PATH=/opt/homebrew/opt/llvm/lib
export DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/opt/llvm/lib
cargo build --offline -p memprove-cli -p memprove-daemon
cargo test --offline --workspace
cargo fmt --all -- --check
cargo clippy --offline --workspace --all-targets -- -D warnings
```

On Ubuntu, `apt-get install libclang-dev` is enough. The integration tests
start a real `memproved` on a Unix socket in a temporary directory, so
they need no installation and touch nothing of yours. CI
(`.github/workflows/ci.yml`) runs these four commands on macOS and Ubuntu.

## What a change must keep

- The ledger is append-only: event payloads, digests, the hash chain and
  the SQLite migrations are never edited in place. A schema change is a new
  migration file.
- Dependencies are pinned (`=x.y.z`) and `Cargo.lock` is committed; adding
  or upgrading one is a separate, explained change.
- The Context Pack, `resume --text`, the hook's statements and the MCP tool
  descriptions are fixed English text with tests that pin their shape; a
  change to them changes those tests in the same commit, and the agent
  guide (`docs/guide.md`) says the same as `--help` does (the tests in
  `crates/memprove-cli/tests/help_e2e.rs` check that).
- A tampered ledger never produces the word `verified` in a Pack, a
  Preview, `resume --text` or a tool result; the shared check is
  `memprove_domain::says_verified`.
- No `#[allow(clippy::too_many_lines)]` is added; split the function.

## Commits and pull requests

- One change per commit, with a title `<area>: <imperative summary>` under
  72 characters (`cli: add init that wires a repository in one step`), and
  a body that says what changed and why.
- Run the narrow tests of what you touched, then the four commands above,
  before opening a pull request. Say in the pull request which commands you
  ran and what they printed.
- The documentation is `README.md`, `docs/guide.md`,
  `docs/architecture.md` and `docs/takeover-eval.md`.

## Licence

By contributing you agree that your contribution is licensed under the
Apache License, Version 2.0 (`LICENSE`).
