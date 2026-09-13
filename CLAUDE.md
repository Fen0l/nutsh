# nutsh

A terminal client for Nutanix Prism Central: it reads the v4 APIs, draws them as tables and
pages, and when something is missing it names the namespace this Prism Central does not serve
rather than showing an empty box. Rust, MIT, one binary called `nutsh`.

This file orients a new session. `README.md` is what the tool does and how to use it;
`CONTRIBUTING.md` holds the engineering invariants and is the one to read before changing
behaviour.

## Build and test

The toolchain is not on `PATH` by default: `export PATH="$HOME/.cargo/bin:$PATH"`.

```sh
make ci                 # the gate: fmt, clippy -D warnings, the test suite, catalog drift
cargo build
cargo xtask gen-catalog # regenerate the catalog from specs/
make release            # stripped binary, with a receipt
```

`make ci` mirrors CI exactly. Run it before calling anything done; a change that has not passed
it is not finished.

Do not build with `--features linux-keyring` unless the machine has libdbus and a running Secret
Service. Without them the build fails, and the file secret store is the default on Linux anyway.

## Layout

| | |
|---|---|
| `src/` | the binary: argument parsing, `ctx`/`cache` subcommands, `--check`, `--snapshot` |
| `crates/catalog` | the static catalog of v4 kinds, generated from `specs/` |
| `crates/config` | config file, contexts, secret storage, atomic private writes |
| `crates/prism` | the HTTP client: auth, TLS, ETags, paging, rate limiting |
| `crates/core` | terminal-free core: store, scheduler, cells, detail, guardrails, cache, log |
| `crates/tui` | ratatui views and the mode state machine; `App` never touches the terminal |
| `crates/mockpc` | in-process mock Prism Central replaying fixtures under the real API paths |
| `xtask` | the catalog generator and the fixture recorder |
| `specs/` | vendored Nutanix OpenAPI documents; nobody here edits them |
| `docs/nutanix-v4-api.md` | what the API does that its own documents do not say |
| `scripts/acceptance.sh` | the read-only pass against a real Prism Central, in phases |

## Files that are generated

`crates/catalog/src/generated.rs` comes from `specs/` plus `crates/catalog/curated.toml` and
`crates/catalog/pages.toml`. Never edit it by hand - change the curation or the generator and
run `cargo xtask gen-catalog`. `make ci`'s last step regenerates it and fails on any diff.

`.github/workflows/release.yml` comes from `dist-workspace.toml` via `dist generate`. Same rule:
change the config, regenerate. CI checks the two agree.

Adding a field to `Kind`, `Action`, `Column` or `Namespace` needs a bootstrap step, because the
generator links the crate whose committed output it is about to replace: seed the new field into
`generated.rs` mechanically first, then generate, then generate again to prove determinism.
Prefer a field with a natural default.

## Snapshots

Tests use `insta`. A snapshot is a rendered frame, and a frame is the only place several rules
are visible at all, so read a changed one by eye before accepting it. `*.snap.new` is never
committed.

## Talking to a real Prism Central

**One login attempt, ever. Never retry a rejected credential.** Each one-shot CLI run is a fresh
process and cannot reuse the previous run's session cookie, so every invocation costs one
authentication, and a loop of them against a bad password is how an account with a lockout
policy gets locked out. Batch what is needed into as few runs as possible.

Never run anything that mutates without being asked for that specific action, on that specific
entity, on the day.

The first invariant in `CONTRIBUTING.md` is the code-level form of this rule, with the tests
that hold it up. Read it before touching `crates/prism`.

## What must not enter the tree

A test fails if a tracked file names real infrastructure: a host name, an address range, a site
or an organisation. `xtask/tests/tree_is_clean.rs` scans every tracked file,
`xtask/tests/fixtures_are_clean.rs` scans the recorded fixtures structurally.

Neither scan can catch a *name* in prose - a cluster name inside an alert message has no shape
to match - so a recorded fixture is reviewed by enumerating distinct values per dotted path, and
a comment never quotes the value it is teaching you to mask. Invent one of the same shape.

Credentials: a password never reaches a log, the cache, a fixture, the config file, `argv` or a
child process's environment. `tests/log.rs` pins the logging half against a live session.

## Conventions

Comments say why a shape is load-bearing, not what the line does, and not the story of how it
came to be. No incident narrative, no references to past bugs, no pointers to private plans or
trackers.

One change per commit, `make ci` green on each. If a fix is worth making, the test that fails
without it is worth writing first.

**Never push. The owner pushes, always.** Commit when asked, and stop there - `git push`, `git
push --tags`, creating a tag that triggers a release, and anything else that moves work to a
remote are the owner's to run. This is not a preference: a tag on this repository publishes
binaries and a Homebrew formula to the public.
