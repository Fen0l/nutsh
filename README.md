<div align="center">

<img src="docs/assets/logo.svg" width="72" height="72" alt="">

# nutsh

**Nutanix Prism Central, in your terminal.**

[![CI](https://github.com/Fen0l/nutsh/actions/workflows/ci.yml/badge.svg)](https://github.com/Fen0l/nutsh/actions/workflows/ci.yml)
[![Audit](https://github.com/Fen0l/nutsh/actions/workflows/audit.yml/badge.svg)](https://github.com/Fen0l/nutsh/actions/workflows/audit.yml)
[![Release](https://img.shields.io/github/v/release/Fen0l/nutsh?include_prereleases&sort=semver)](https://github.com/Fen0l/nutsh/releases)
[![Downloads](https://img.shields.io/github/downloads/Fen0l/nutsh/total)](https://github.com/Fen0l/nutsh/releases)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

[Website](https://nutsh.dev) ·
[Usage guide](https://nutsh.dev/usage) ·
[Keys](docs/keys.md) ·
[Compared with ncli, acli, nuclei](https://nutsh.dev/compare) ·
[Changelog](CHANGELOG.md)

</div>

Nutsh is a keyboard-driven terminal UI for Prism Central, in the shape of [k9s](https://k9scli.io).
It runs on your laptop and talks to the v4 REST API over HTTPS: no SSH, no agent, nothing
installed on the cluster. You get every resource kind behind one prompt, names instead of
UUIDs, and a guardrail in front of anything that changes state.

It is for the people who live in Prism Central: administrators and SREs who want the VM table,
the failed tasks and the alerts without a browser tab, consultants who move between several
Prism Centrals a day, and Community Edition home labs.

```
╭ nutsh ─────────────────────────────────────────────────────────────────────────────────────╮
│Context:    lab                                                  ⏎ drill        y detail    │       clusters 1  hosts 1
│PC:         pc.example.com  pc.2024.3                           S sort         w wide      │         vms 3  on/off 3/0
│Cluster:    <all>                                               ←→ columns     ^r refresh   │           alerts ⚠ 2  ✖ 2
│Kind:       Compute & Storage › VMs                             ^t every       ^x stop      │         tasks ▶ 4 running
│Count:      2 of 3                                               a actions     ^c quit      │
╰────────────────────────────────────────────────────────────────────────────────────────────╯  v0.0.2-beta3 · pc.2024.3
╭ nutsh ───────────────╮╭ Virtual Machines [2 of 3] /web ──────────────────────────────────────────────────────────────╮
│  ▸ Dashboard         ││  NAME                                     POWER  CLUSTER      HOST        IP            MEM  │
│  ▾ Compute & Storage ││▌ web-01                                   ON     lab-cluster  ahv-node-1  203.0.113.11  8 GiB│
│      Overview        ││  web-02                                   OFF    lab-cluster  -           -             4 GiB│
│    • VMs [3]         ││                                                                                              │
│      ESXi VMs        ││                                                                                              │
│      Templates       ││                                                                                              │
│      Images          ││                                                                                              │
╰───────────────── ↓35 ╯╰──────────────────────────────────────────────────────────────────────────────────────────────╯
/web█
                                                                                        2.1 req/s · 68% cached  ● live
```

*The VM table, filtered with `/`. Every frame in this README comes from the test suite's mock
Prism Central, not from a real one.*

## Status

**0.0.2-beta3.** One author, a few months of daily use against one Prism Central (pc.7.6) and
a mock built from the published pc.2024.3 API specs. Nobody else has run it for a week yet.
Expect rough edges, and expect the version number to move.

Before you point it at anything real, know what it holds and does:

- It stores a Prism Central password: in the OS keyring where one exists, otherwise in a
  `0600` file under `$XDG_STATE_HOME/nutsh/secrets/`. Never in the config file, `argv`, a log
  or the cache.
- A rejected credential is presented **once**, then everything stops. There is no retry path
  that can reach an authenticating request. This program has locked out a lab account in its
  past; the rules that stop that from happening again are pinned by tests, and
  [CONTRIBUTING.md](CONTRIBUTING.md) names them.
- `--readonly` refuses every mutation before the wire. Use it for a first look.
- Requests are paced from the rate limit the Prism Central advertises, never faster.
- The on-disk cache is an inventory of your infrastructure, `0600`, per context;
  `nutsh cache clear` removes it.

macOS and Linux today. No Windows build yet; WSL works.

## Install

| | |
|---|---|
| **Homebrew**, macOS and Linux | `brew install Fen0l/tap/nutsh` |
| **Shell installer**, any distribution | `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Fen0l/nutsh/releases/download/v0.0.2-beta3/nutsh-installer.sh \| sh` |
| **Binaries** | [Releases](https://github.com/Fen0l/nutsh/releases): macOS and Linux, x86_64 and arm64, each archive with its SHA-256. Linux comes as `gnu` and as `musl`, a static binary for any distribution. |
| **From source** | `git clone https://github.com/Fen0l/nutsh && cd nutsh && make release`, with a Rust toolchain. The binary lands at `target/release/nutsh`. |

`releases/latest/` skips pre-releases, so the installer URL names the version until there is a
stable one. Not on crates.io yet.

## First five minutes

```sh
nutsh ctx add home --host pc.example.com --username admin    # one Prism Central, one context
nutsh ctx login home                                         # asks for the password once, checks it, stores it
nutsh --readonly                                             # the Dashboard, nothing can change
```

Then, inside:

- `:` opens the palette. Type `vm`, `alerts`, `tasks`, or any of 232 kinds by name or alias.
- `/` filters the rows in front of you. `:search web` looks everywhere, then asks the Prism Central.
- `⏎` drills into a row's children; `y` opens its detail; `esc` goes back.
- `a` lists what can be done to the row, greyed where you may not, with the reason beside it.
- `:ctx` switches to another Prism Central. `?` shows every key. `q` quits.

If the certificate is rejected, that is normal: Prism Central's default certificate is
self-signed *and* marked as a CA, which no modern TLS library accepts as a server certificate.
Install a signed one, or add the context with `--insecure` (the header then says so).

Tab completion for the shell: `source <(nutsh completions zsh)`; `bash` and `fish` likewise.

## What it does

- **Every kind, one prompt.** 232 kinds across all 20 v4 namespaces. 74 have hand-picked
  columns; the rest get a layout derived from the schema. Wherever the API returns a UUID you
  see the name.
- **Pages.** Attention puts unresolved alerts, failed tasks, powered-off VMs and the hosts side
  by side. Disaster Recovery gathers remote Prism Centrals, policies, plans and recent jobs.
- **Guarded actions.** 478 of them. Read-only refuses before the wire, delete makes you type
  the name, bulk stops at five rows, your own `[[guardrails]]` deny or confirm by context, kind
  and action. Every attempt lands in `:journal`, refused ones included.
- **Honest emptiness.** When something is missing it says why - which API namespace this
  Prism Central does not serve, or which version a resource needs - instead of an empty box.
- **Several Prism Centrals.** A context per environment, switched in one command.
- **Useful outside the TUI.** `nutsh --check` prints what your Prism Central serves.
  `nutsh vm --snapshot` prints one frame; `--format csv|json` prints the table for scripts.
- **Gentle on the Prism Central.** Cached per context, ETag revalidation, paced per endpoint
  from the advertised tier. `:activity` shows every request the session made.

The full key map and every palette command are in [docs/keys.md](docs/keys.md); the config
file, cache, logging and pacing in [docs/reference.md](docs/reference.md).

## Proof

- 1,100+ tests, run on Linux and macOS on every pull request, plus a weekly `cargo audit` and
  CodeQL. `make ci` is the gate and mirrors CI exactly.
- The lockout rules, the pacing rules and the "never carries neither a credential nor a
  session" rule each have a named test that fails if they are undone.
- Verified against: **pc.7.6** (the author's lab, continuously) and a mock built from the
  **pc.2024.3** specs (every test). Other versions are negotiated at connect time - `--check`
  prints what yours answered - but have not been run by anyone yet. If you run it against
  another version, [say so](https://github.com/Fen0l/nutsh/issues/new?template=compat.yml).
- v4 coverage, generated from the catalog and checked for drift in CI:

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

**One thing to know about the v4 API.** Prism Central's own console still reads several entity
types through the older v3 `groups` endpoint, and those have no populated v4 collection behind
them. So a pane can say `no recovery plans` while the console lists several. **An empty v4
collection means the v4 API has none; it does not prove the Prism Central has none.**
[docs/nutanix-v4-api.md](docs/nutanix-v4-api.md) has the cases and why v3 is not implemented.

## Alternatives

`ncli` and `acli` run on a CVM over SSH and see one cluster. `nuclei` runs on the Prism Central
VM over SSH. The console is a browser tab. Terraform and Ansible declare state; they do not
show it. Nutsh is the interactive one that runs on your laptop, over HTTPS, with nothing
installed anywhere. [nutsh.dev/compare](https://nutsh.dev/compare) goes tool by tool, and says
when Nutsh is the wrong choice.

## Documentation

- [Usage guide](https://nutsh.dev/usage): first connection, the screens, guardrails, the cache
- [docs/keys.md](docs/keys.md): every key, every palette command, every direct action key
- [docs/reference.md](docs/reference.md): the config file, cache, logging, request pacing
- [docs/coverage.md](docs/coverage.md): v4 coverage per namespace, generated
- [docs/nutanix-v4-api.md](docs/nutanix-v4-api.md): what the v4 API does that its own docs do not say
- [SECURITY.md](SECURITY.md): what is stored where, and how to report a defect privately
- [CHANGELOG.md](CHANGELOG.md): what changed, release by release

## Contributing

Issues and pull requests are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) has the build, the
tests, and the invariants that stop somebody undoing a fix without knowing what it prevented.
Read it before touching the HTTP client: this program talks to systems with lockout policies.

Built with a lot of AI assistance, and said so plainly: the author wrote the mockups and the
rules about what it must never do, and Claude wrote most of the rest. Every change is reviewed
by a human before it goes near a lab, and the safeguards against an accidental deletion were
designed by that human. The tests and the guardrails are in the repository, and they are what
to judge it on.

## License

MIT. The text is in [LICENSE](LICENSE).

Nutanix, Prism Central and AHV are trademarks of Nutanix, Inc. This project is independent, and
not affiliated with or endorsed by Nutanix.
