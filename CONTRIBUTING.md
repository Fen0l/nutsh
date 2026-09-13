# Contributing to nutsh

Patches are welcome. This file is the short version of everything a maintainer needs to know
before changing something here: how to build it, what the gate is, which files are generated,
and which of the shapes in this code are load-bearing rather than incidental.

## Build

Rust, pinned by `rust-toolchain.toml`. If `cargo` is not on the path, it usually lives in
`~/.cargo/bin`:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo build
```

`make release` builds, strips, and prints a receipt: the absolute path and byte count of
`target/release/nutsh`, so "it built" is something you can check rather than take on trust.

**Never build with `--features linux-keyring` unless you actually have a Secret Service.** It
needs libdbus and a running daemon. Under WSL2 there is neither, and the default Linux build
uses the file store instead.

## The gate

```sh
make ci
```

Four steps, and CI runs the same four on Linux and macOS:

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. the catalog drift check

A change is not finished until all four pass. There is no separate lint job to defer to and no
warning budget: clippy is `-D warnings`, so a warning is a failure.

## Files you must not edit by hand

**`crates/catalog/src/generated.rs` is generated.** `cargo xtask gen-catalog` writes it from the
OpenAPI documents in `specs/`, and CI fails if the committed file differs from what the
generator produces. Editing it by hand works exactly until the next regeneration silently
reverts you. Everything a person is meant to decide lives beside it and is hand-written:

- `crates/catalog/curated.toml` for display names, aliases, columns, detail sections, actions
  and poll intervals
- `crates/catalog/nav.toml` for the menu
- `crates/catalog/pages.toml` for the feature pages

`specs/` itself is vendored. `specs/sync.py` downloads it from the vendor (standard library plus
`httpx`: `python3 specs/sync.py -v`, or `uv run --with httpx specs/sync.py -v`). Follow any sync
with `cargo xtask gen-catalog`.

## Snapshots

Frame snapshots are [`insta`](https://insta.rs). To re-record:

```sh
INSTA_UPDATE=always cargo test --workspace
```

**Then read the diff, line by line, before you stage it.** Accepting snapshots in bulk is how a
regression gets committed as an expectation. A rendered frame is the only place several of the
rules below are visible at all: a clipped help line, a column that lost its name, a detail pane
that half-empties on a poll. None of those fails a unit test. They fail by looking wrong in a
snapshot that somebody read.

`insta` writes `*.snap.new` beside a snapshot it could not match. Never commit one; it means a
test failed and the failure has not been looked at.

## Fixtures and the mock

`crates/mockpc` is a Prism Central built from the published specs, and every test runs against
it. Two kinds of fixture feed it:

- `crates/mockpc/fixtures/` is curated by hand, to show one shape clearly.
- `crates/mockpc/fixtures-lab*/` are recordings from real Prism Centrals, redacted on the way
  in by `cargo xtask record --host PC --username U --kinds vm,cluster`.

Tests pick whichever shows the shape under test. Use a curated fixture when the point is the
rule; use a recording when the point is what a real API actually sends, which is often not what
the spec says.

The recorder refuses to write a file that still holds a real address, e-mail, MAC or the host
name it recorded from, and `xtask/tests/fixtures_are_clean.rs` re-runs that scan over every
committed fixture. `xtask/tests/tree_is_clean.rs` does the same job for prose, over every file
`git` tracks, because the identifiers that got into this repository got in through task notes
and doc comments rather than through fixtures. If it fails, it prints the file, the line and the
token. Replace the token with a documentation-safe value: an RFC 5737 address (`192.0.2.x`,
`198.51.100.x`, `203.0.113.x`), an `example.com` or `.invalid` host name, a neutral cluster
name. Do not add an exception.

## Running it

- `nutsh --check` probes every API namespace and says which this Prism Central serves.
- `nutsh --info` prints catalog statistics and the config path.
- `nutsh [KIND] --snapshot --size COLSxROWS` renders one frame to stdout and exits. This is the
  headless way to see a frame, and it is what most of the test suite does. `KIND` may be a kind
  id, an alias, a page id or a display-name prefix; with none, a bare run opens the Dashboard.
  The default size is `120x40`.
- `nutsh ctx add|login|use|list|show|remove` manage contexts in
  `$XDG_CONFIG_HOME/nutsh/config.toml` (`NUTSH_CONFIG` overrides). Tests point `NUTSH_CONFIG`
  and `XDG_STATE_HOME` at temporary directories and set `NUTSH_KEYRING=0`.
- `nutsh cache info|clear` inspects and removes the per-context cache under
  `$XDG_STATE_HOME/nutsh/cache/`; `--no-cache` skips it for one run.
- `NUTSH_LOG=off|error|warn|info|debug|trace`, or any `tracing_subscriber` filter string, turns
  logging on for one run. `log` in the config file and `:log` in the app do the same, in that
  order of precedence, and the default is off. CLI commands log to stderr; the TUI and
  `--snapshot` log to `$XDG_STATE_HOME/nutsh/logs/nutsh.log`, capped and rolled. `debug` puts
  every request on one line with its status, elapsed time and credential path, which is how you
  find out which request got a 401.

## Testing against a real Prism Central

Read the next section first, particularly the first invariant. Then: use one login, batch what
you need into as few runs as possible, and never retry a credential that was refused. Each
one-shot CLI run is a fresh process and cannot reuse the previous run's session cookie, so every
invocation costs one authentication.

`scripts/acceptance.sh` is the read-only pass, in phases, so the cost in authentications is
chosen rather than discovered. `docs/nutanix-v4-api.md` is what the API does that its own
documents do not say.

## Invariants

Several of these are invisible to CI in the sense that the code compiles and the tests pass
right up until you remove the one test that pins it. They are here so that a change does not
quietly undo one.

**A rejected credential is terminal, everywhere.** Four independent paths can see a 401: the
scheduler, the stats poller, the name resolver and the socket sampler. Any one of them retrying
is an authentication failure per cycle, for as long as the program runs, which on an account
with a lockout policy is a lockout. Each has a test pinning that it does not retry:
`crates/prism/tests/auth.rs::a_401_that_arrives_midway_latches_too`,
`crates/prism/tests/auth.rs::the_valve_latches_and_the_session_reports_itself_stopped`,
`crates/core/tests/scheduler.rs::a_401_midway_stops_the_subscription_and_keeps_the_rows`. A
retry loop that can reach an authenticating request is not a resilience improvement here; it is
an account lockout.

**`crates/prism/tests/auth.rs::no_request_ever_carries_neither_a_credential_nor_a_session`.** A
request carrying neither authenticates with nothing. Its certain 401 reads as an expiry, which
drops a good session and spends a second password on a renewal. Do not weaken it.

**Requests are paced from what the Prism Central advertises, never from a constant.** The
`x-ratelimit-*` headers ride every 200 and are declared in no spec file, so they are only
found by reading a response. Each endpoint is paced by the tier that endpoint advertised, not by
a single number for the host:
`crates/prism/tests/ratelimit.rs::an_endpoint_is_paced_by_the_tier_that_endpoint_advertised`,
`crates/prism/tests/ratelimit.rs::we_never_exceed_the_rate_the_prism_central_advertises`. The
bucket is a sliding window on purpose: a fixed window lets five requests out in one second on a
tier of three, by rolling at the seam
(`crates/prism/src/bucket.rs::a_tier_is_never_exceeded_across_a_window_seam`).

**A credential never reaches a log, the cache, a fixture, the config file, `argv`, or a child
process's environment.** Passwords go to the keyring, or to the file store under
`$XDG_STATE_HOME/nutsh/secrets/`, and nowhere else. `tests/log.rs` pins the logging half by
taking the real header values off the wire and asserting they appear nowhere in the output at
maximum verbosity. Only this workspace's own crates can reach the log writer, which is why
`tracing-log` is not a dependency:
a crate that logs through the `log` crate would put headers in a file.

**`$select` comes from the scheduler's `opts.select` and never from `list_page_at`.** `can_i`
reaches the API through `list_page_at` with default options. Narrowing there greys every action
on every kind, silently, because the permission probe comes back without the fields it judges
on. `crates/core/tests/scheduler.rs::a_list_cycle_carries_the_kinds_select_and_nothing_else_does`.

**The detail composes from the list row.** A list cycle replaces the row map wholesale, so
anything that narrows a list must still leave the detail its own whole row, or the pane
half-empties on every poll:
`crates/core/tests/scheduler.rs::a_list_cycle_takes_the_whole_row_off_rows_and_leaves_it_on_the_detail`
and `crates/core/src/store.rs::a_list_cycle_does_not_take_the_whole_row_back_off_the_detail`.

**A mouse click can never fire a mutating action or answer a confirm dialog.** Only
`Mode::{Command, Picker, Skins}` record a hit. Anything destructive is reached from the keyboard,
deliberately.

**A reference's body path is the object, not the id inside it**, unless the path itself ends in
`.extId` or `.uuid`, which the generator also emits. `build_body` reads the last segment to tell
them apart. A bare string where the schema wants an object is a wrong body, and an update merges
it over the object the Prism Central already has.

**`prefill` returning `None` means "not an edit", and `Form::over` reads that as permission to
seed.** An edit that found no values is `Some` and possibly empty. The difference between those
two is a VM's disks: read it wrong and an update invents values over rows it was never given.

**The `?` overlay has zero slack**: fourteen lines, the longest exactly 68 cells. It cannot
grow, because a taller box covers a title that `crates/tui/tests/navigation.rs` asserts on. A
clipped line leaves no trace on a rendered frame, so `ui::tests::the_help_text_fits_its_box` is
the only thing that can catch it. Adding a key means folding two rows onto one, and saying which.

## Commits

One change per commit, with a message that says what changed and why rather than restating the
diff. `make ci` green on each. If a fix is worth making, the test that fails without it is worth
writing first, and it should fail against the commit before the fix.
