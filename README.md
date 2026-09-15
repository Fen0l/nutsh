# nutsh

[![CI](https://github.com/Fen0l/nutsh/actions/workflows/ci.yml/badge.svg)](https://github.com/Fen0l/nutsh/actions/workflows/ci.yml)
[![Audit](https://github.com/Fen0l/nutsh/actions/workflows/audit.yml/badge.svg)](https://github.com/Fen0l/nutsh/actions/workflows/audit.yml)
[![Release](https://img.shields.io/github/v/release/Fen0l/nutsh?include_prereleases&sort=semver)](https://github.com/Fen0l/nutsh/releases)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
![Rust](https://img.shields.io/badge/rust-1.98.1-orange)

A terminal UI for Nutanix Prism Central, in the shape of [k9s](https://k9scli.io). Browse your
infrastructure as tables you can move around in, drill into a row, run guarded actions, and
switch between environments without leaving the keyboard.

When something is missing it tells you why - which API namespace this Prism Central does not
serve, or which version a resource needs - instead of showing you an empty box.

```
 Virtual Machines [847]                                    3.2 req/s · 41% cached
 NAME             STATE   CLUSTER      HOST         vCPU  MEMORY   IP
 web-01           ON      prod-01      node-03         4   8 GiB   10.0.4.11
 web-02           ON      prod-01      node-01         4   8 GiB   10.0.4.12
 db-primary       ON      prod-01      node-02        16  64 GiB   10.0.4.20
 backup-runner    OFF     prod-02      -               2   4 GiB   -
```

Version 0.0.2-beta2. Everything here is implemented and tested; none of it has been through a
week of ordinary use by anybody but its author. [CHANGELOG.md](CHANGELOG.md) has the details.

## v4 coverage

<!-- coverage:start -->
**Breadth** - how much of the v4 API the catalog models.

| measure | count | share |
|---|---:|---:|
| operations the pinned specs declare | 1023 | |
| modelled by the catalog | 880 | 86% |
| namespaces | 20 | 100% |
| kinds | 232 | |
| openable from the palette | 135 | 58% |
| reached by drilling into a parent | 83 | 36% |
| need a parameter nutsh cannot supply | 14 | 6% |
| actions | 478 | |

**Depth** - the curated path, and the explorer beneath it.

| measure | count | share of kinds |
|---|---:|---:|
| kinds the curated menu opens, every one with hand-picked columns | 74 | 32% |
| kinds with hand-picked columns, in all | 78 | 34% |
| kinds with a written detail layout | 12 | 5% |
| kinds left to the namespace explorer, columns derived from the schema | 154 | 66% |

[docs/coverage.md](docs/coverage.md) breaks this down per namespace.
<!-- coverage:end -->

## Install

```sh
brew install Fen0l/tap/nutsh
```

Or take a binary directly:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Fen0l/nutsh/releases/download/v0.0.2-beta2/nutsh-installer.sh | sh
```

`releases/latest/` skips pre-releases, so the URL names the version until there is a stable one.

Builds are published for macOS and Linux, x86_64 and arm64. The Linux archives come in two
flavours: `gnu`, and `musl` for a static binary that runs on any distribution.

Or build it, which needs a Rust toolchain:

```sh
git clone https://github.com/Fen0l/nutsh && cd nutsh
make release     # leaves the binary at target/release/nutsh
```

## First run

A context names one Prism Central, a username, and optionally a cluster to scope to.

```sh
nutsh ctx add home --host pc.example.com --username admin --cluster prod-01
nutsh ctx login home     # asks once, checks it against the PC, then stores it
nutsh                    # the Dashboard
```

The password is verified before anything is written, and stored in your system keyring where
there is one, or in a file only you can read where there is not. It never reaches the command
line, the config file, the cache or a log.

**If the certificate is rejected**, that is normal: Prism Central's default certificate is
self-signed *and* marked as a CA, which no modern TLS library will accept as a server
certificate - `--ca-bundle` cannot fix that one. Either install a properly signed certificate on
the Prism Central, or add the context with `--insecure`.

Useful from the shell, without entering the TUI:

```sh
nutsh vm                 # open straight to a kind, by name, alias or id
nutsh --check            # connect, probe every namespace, print what this PC serves
nutsh vm --snapshot      # render one frame to stdout and exit
nutsh ctx list           # your contexts; `use`, `show` and `remove` do what they say
```

## Getting around

A bare `nutsh` opens the **Dashboard**. The menu on the left holds seven curated groups -
Compute & Storage, Network & Security, Hardware, Monitoring, Policies, Admin Center, Disaster
Recovery - and each opens a **page** of panes before you reach the resources under it. Below
them, everything else, grouped by API namespace. Nothing appears twice.

A **table** is one kind of thing. `⏎` drills into a row's children, `y` opens a readable detail,
`a` opens the actions for that row.

| key | |
| --- | --- |
| `:` | palette - any kind, page or command |
| `/` | filter what is in front of you |
| `?` | the full key list |
| `j` `k` `g` `G` | move, first, last |
| `enter` | drill in, or open the detail |
| `esc` | back |
| `y` | detail; `Y` and `J` for raw YAML and JSON |
| `a` | actions for this row |
| `space` | mark a row, so `a` acts on all the marked ones |
| `S` `w` | sort by the next column, show twenty columns |
| `tab` `ctrl-b` | focus the menu, hide the menu |
| `ctrl-r` `ctrl-t` | refresh now, change how often this view polls |
| `q` | quit |

Actions have their own keys, shown beside each one in the `a` menu. On a VM: `p`/`P` power on
and off, `d`/`D` shut down via ACPI or guest tools, `C` clone, `s` snapshot, `ctrl-d` delete.

The mouse works too - click a row, scroll the wheel. A click can never fire a mutating action or
answer a confirmation; those are keyboard-only, deliberately.

## The palette

`:` opens one ranked list of everything, completing as you type.

| | |
| --- | --- |
| `:ctx [name]` | switch environment, or open the Contexts screen |
| `:search <term>` | search by name, address or id: what is loaded first, then the Prism Central |
| `:can-i <action> <kind>` | whether your account may do that, and what it needs |
| `:journal` | everything this session attempted, and what answered |
| `:activity` | every request this session made, and every table it holds with when it last polled |
| `:skin [name]` | pick a colour scheme |
| `:settings` | every setting, its value, and where that value came from; the version line in the header opens it too |
| `:hide` `:show` | keep things out of the menu, or put them back |

`/` filters the rows in front of you and makes no request. `:search` answers from what is loaded,
then asks the Prism Central for every kind that can be filtered by name, and the line under the
results counts the kinds asked and answered as they land.

## Not doing something by accident

```sh
nutsh --readonly         # refuse every mutation this session
```

`readonly` can also be set for one context, or globally in the config file. Beyond that,
guardrails match on context, kind and action, and either refuse outright with your own reason,
force a confirmation, or cap how many rows one action may touch:

```toml
[[guardrails]]
contexts = ["prod"]
actions  = ["delete"]
deny     = true
reason   = "deletions in prod go through change control"
```

Anything refused says so in the menu beside it, so nothing is ever offered and then taken away.
Deleting asks you to type the name; powering off asks for a plain yes.

## Configuration

`~/.config/nutsh/config.toml`, written by the app itself when you change something. `:settings`
inside the app lists every setting, its current value, and where that value came from.
Passwords are never in it.

The full file - skins, per-kind refresh rates, guardrails, the menu - is documented in
[docs/reference.md](docs/reference.md), along with the cache, the log, and how requests are
paced.

## One thing to know about the v4 API

nutsh speaks only the v4 REST API. Prism Central's own console does not - for several entity
types it still reads the older v3 `groups` endpoint, and those types have no populated v4
collection behind them.

So a pane can say `no recovery plans` while the console lists several. **An empty v4 collection
means the v4 API has none; it does not prove the Prism Central has none.** Recovery plans, DR
jobs, projects, blueprints and marketplace items are the known cases.

Buckets are a separate case with the same symptom. The list is scoped to a namespace by a header
nutsh does not send, so only the local namespace appears and federated ones are not listed.
[docs/reference.md](docs/reference.md) has the detail, and why v3 is not implemented.

## More

- [docs/reference.md](docs/reference.md) - config, cache, logging, request pacing, v4 coverage
- [docs/nutanix-v4-api.md](docs/nutanix-v4-api.md) - what the v4 API does that its own docs do not say
- [CONTRIBUTING.md](CONTRIBUTING.md) - building, testing, and the invariants that matter
- [SECURITY.md](SECURITY.md) - reporting a vulnerability

## License

MIT. The text is in [LICENSE](LICENSE).

Nutanix and Prism Central are trademarks of Nutanix, Inc. This project is not affiliated with or
endorsed by Nutanix.
