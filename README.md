<div align="center">

<img src="docs/assets/logo.svg" width="72" height="72" alt="">

# nutsh

**Nutanix in your terminal.**<br>
A keyboard-first terminal UI for Nutanix Prism Central, in the shape of [k9s](https://k9scli.io).

[![CI](https://github.com/Fen0l/nutsh/actions/workflows/ci.yml/badge.svg)](https://github.com/Fen0l/nutsh/actions/workflows/ci.yml)
[![Audit](https://github.com/Fen0l/nutsh/actions/workflows/audit.yml/badge.svg)](https://github.com/Fen0l/nutsh/actions/workflows/audit.yml)
[![Release](https://img.shields.io/github/v/release/Fen0l/nutsh?include_prereleases&sort=semver)](https://github.com/Fen0l/nutsh/releases)
[![Downloads](https://img.shields.io/github/downloads/Fen0l/nutsh/total)](https://github.com/Fen0l/nutsh/releases)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
![Rust](https://img.shields.io/badge/rust-1.98.1-orange)

[Website](https://nutsh.dev) ·
[Usage guide](https://nutsh.dev/usage) ·
[Keys](docs/keys.md) ·
[Install](#install) ·
[Compared with ncli, acli, nuclei](https://nutsh.dev/compare) ·
[Changelog](CHANGELOG.md)

</div>

<p align="center">
  <img src="docs/assets/demo.gif" width="100%" alt="A recorded Nutsh session: the Dashboard, the VM table from the palette, a filter narrowing the rows, a VM's action menu, a switch to a second Prism Central, and back to the Dashboard.">
</p>

Nutsh runs on your own machine and speaks the Prism Central **v4 REST API over HTTPS**. No SSH
session, no agent, nothing installed on the cluster. Every resource kind sits behind one colon
prompt, so an obscure networking resource behaves the same way the VM table does, and anything
that changes state has to get past a guardrail first. When something is missing it tells you
why, which API namespace this Prism Central does not serve or which version a resource needs,
instead of showing you an empty box.

> **Status:** 0.0.2-beta2, macOS and Linux. Everything here is implemented and tested; none of
> it has been through a week of ordinary use by anybody but its author.

## Features

- <kbd>:</kbd> **Every kind, one prompt.** 232 kinds across all 20 v4 namespaces, reachable by name or alias. 74 have hand-picked columns; the rest get a layout derived from the schema.
- <kbd>:ctx</kbd> **Several Prism Centrals.** A context is one environment. Switch in one command. The password goes to the keyring or a 0600 file, never to the config, argv or a log.
- <kbd>⏎</kbd> **Relationships, by name.** Drill from a row into the kinds hanging off it. Wherever the API hands back a UUID, you see the name.
- <kbd>a</kbd> **478 actions, all guarded.** Read-only mode refuses before the wire. Delete makes you type the name. Bulk stops at five rows. Every attempt lands in the session journal, refused ones included.
- <kbd>:can-i</kbd> **Permission preflight.** Checks your IAM roles against what an action needs. If the lookup fails it says *unknown*, never *no*.
- <kbd>⇥</kbd> **Pages.** Eight screens put several panes side by side. Disaster Recovery gathers remote Prism Centrals, policies, plans and recent jobs in one place.
- <kbd>/</kbd> **Filter here, search everywhere.** `/` narrows the rows in front of you without a request. `:search` asks every loaded kind, then the Prism Central itself.
- <kbd>:skin</kbd> **Seventeen skins, and a working mouse.** Catppuccin, gruvbox, nord, dracula, tokyo-night and a dozen more. Clicks work; shift-drag still selects text.
- `--check` **Version negotiation.** Asks each namespace which version it serves and steps down on a 404. One binary from the pc.2024 line through PC 7.x.
- `--snapshot` **Useful outside the TUI.** One frame to stdout, no colour codes, so two runs against the same state diff clean. Handy in scripts, CI and incident notes.
- **Gentle on the Prism Central.** Responses cached on disk per context, ETag revalidation, per-namespace pacing and a 30 req/s ceiling. The status line shows the rate and what the cache absorbed.

<details>
<summary><strong>More screens</strong>: the action menu, the palette, a page</summary>

<br>

<kbd>a</kbd> on a VM. The bracketed key runs that action straight from the table.

```
╭ nutsh ──────────────────────────────╭ act on web-01 ──────────────────────────────────────────────────╮
│Context:    lab                      │▌ Power on                                                    [p]│ters 1  hosts 1
│PC:         pc.example.com  pc.2024.│  Power off (cuts power)                                      [P]│s 3  on/off 3/0
│Cluster:    <all>                    │  Power cycle (cuts power)                                       │alerts ⚠ 2  ✖ 2
│Kind:       Compute & Storage › VMs  │  Reset (cuts power)                                             │sks ▶ 4 running
│Count:      3                        │  Shut down (ACPI, asks the guest)                            [d]│
╰─────────────────────────────────────│  Shut down (guest tools)                                     [D]│ta2 · pc.2024.3
╭ nutsh ───────────────╮╭ Virtual Mach│  Reboot (ACPI, asks the guest)                               [r]│──────────────╮
│  ▸ Dashboard         ││  NAME       │  Reboot (guest tools)                                        [R]│        MEM   │
│  ▾ Compute & Storage ││▌ web-01     │  Migrate to another cluster                                     │113.11  8 GiB │
│      Overview        ││  web-02     │  Migrate to host                                             [m]│        4 GiB │
│    • VMs [3]         ││  db-01      │  Clone                                                       [C]│        32 GiB│
│      ESXi VMs        ││             │  Create recovery point                                       [s]│              │
│      Templates       ││             │  Revert to a recovery point                                     │              │
│      Images          ││             │  Delete                                                 [ctrl-d]│              │
│      OVAs            ││             │  ──────────────────────── raw API names ────────────────────────│              │
│      VM Profiles     ││             │  add-custom-attributes                                          │              │
│      Guest Customizat││             │  assign-owner                                                   │              │
╰───────────────── ↓24 ╯╰─────────────╰─────────────────────────────────────────────────────────────────╯──────────────╯
 type to filter  ↑↓ move  enter run  esc close
```

<kbd>:</kbd> then `vm`. Kinds, pages and commands in one ranked list, with the catalog id beside each.

```
╭ nutsh ───────────────╮╭ Virtual Machines [3] ────────────────────────────────────────────────────────────────────────╮
│  ▸ Dashboard         ││╭ kinds & commands (⇥ complete · ↑↓ · ⏎) ────╮R  CLUSTER      HOST        IP            MEM   │
│  ▾ Compute & Storage │││▌ Virtual Machines          …m.ahv.config.Vm│   lab-cluster  ahv-node-1  203.0.113.11  8 GiB │
│      Overview        │││  ESXi Virtual Machines     ….esxi.config.Vm│   lab-cluster  -           -             4 GiB │
│    • VMs [3]         │││  VM Stats                 needs a parameter│   lab-cluster  ahv-node-1  -             32 GiB│
│      ESXi VMs        │││  VM Stats                 needs a parameter│                                                │
│      Templates       │││  VM Guest Customization Pr …mizationProfile│                                                │
│      Images          │││  VM Profiles               …onfig.VmProfile│                                                │
│      OVAs            │││  VM Recovery Points        …VmRecoveryPoint│                                                │
│      VM Profiles     │││  VM Anti Affinity Policies …iAffinityPolicy│                                                │
│      Guest Customizat││╰─────────────────────────────────────── ↓22 ╯                                                │
╰───────────────── ↓33 ╯╰──────────────────────────────────────────────────────────────────────────────────────────────╯
:vm█
```

The Disaster Recovery page: four panes, one screen, <kbd>O</kbd> opens any of them as a full table.

```
╭ nutsh ─────────── ↑3 ╮╭ Registered Prism Centrals [1] ─────────────────────────────────────╮╭ Summary ───────────────╮
│      VMs             ││  NAME       PC VERSION   CLUSTERS  STATE   CONNECTIVITY  REGISTERED││Policies               3│
│      ESXi VMs        ││▌ pc-dr      pc.7.6       3         ACTIVE  CONNECTED     11d       ││Recovery plans         2│
│      Templates       ││                                                                    ││Recovery points        5│
│      Images          │╰────────────────────────────────────────────────────────────────────╯│Recovery jobs          2│
│      OVAs            │╭ Protection Policies [3] ───────────────────────────────────────────╮│Sampled VMs            3│
│      VM Profiles     ││  NAME                                   SITES      RPO   CATEGORIES││  in sync              1│
│      Guest Customizat││  gold-sync                              pc-lab +1  0s    1         ││  syncing              1│
│      Storage Containe││  silver-async                           pc-lab +1  1h    1         ││  out of sync          1│
│      Volume Groups   ││  bronze-local                           pc-lab     6h    1         ││                        │
│      Storage Policies││                                                                    ││                        │
│      Categories      │╰────────────────────────────────────────────────────────────────────╯╰────────────────────────╯
│      Catalog Items   │╭ Recovery Plans [2] ──────────────────────────────────────────────────────────────────────────╮
│  ▸ Network & Security││  NAME                                                     PRIMARY SITE  RECOVERY SITE  PAUSED│
│  ▾ Data Protection   ││  dr-tier1                                                 pc-lab        pc-dr          ✗     │
│    • Disaster Recover││  dr-tier2                                                 pc-lab        pc-dr          ✗     │
│      Protection Polic│╰──────────────────────────────────────────────────────────────────────────────────────────────╯
│      Recovery Plans  │╭ Recent DR jobs [2] ──────────────────────────────────────────────────────────────────────────╮
│      Recovery Plan Jo││  NAME                                      ACTION            STATUS     PLAN      %     START│
│      Recovery Points ││  failover-test-04                          Test failover     SUCCEEDED  dr-tier1  100%  3h   │
│      Recovery Point S││  failover-test-03                          Planned failover  FAILED     dr-tier2  40%   2d   │
╰───────────────── ↓19 ╯╰──────────────────────────────────────────────────────────────────────────────────────────────╯
 :palette  ^b:menu  ⇥:pane  O:open  a:actions  ⏎:detail  esc:menu  ?:help
```

</details>

## Quick start

```sh
brew install Fen0l/tap/nutsh                                   # or the shell installer below
nutsh ctx add home --host pc.example.com --username admin      # one Prism Central, one context
nutsh ctx login home                                           # asks once, checks it, stores it
nutsh                                                          # the Dashboard
nutsh vm                                                       # or straight to a kind
```

**If the certificate is rejected**, that is normal. Prism Central's default certificate is
self-signed *and* marked as a CA, which no modern TLS library will accept as a server
certificate, and `--ca-bundle` cannot fix that one. Either install a properly signed certificate
on the Prism Central, or add the context with `--insecure`.

## Install

| | |
|---|---|
| **Homebrew**, macOS and Linux | `brew install Fen0l/tap/nutsh` |
| **Shell installer**, any distribution | `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Fen0l/nutsh/releases/download/v0.0.2-beta2/nutsh-installer.sh \| sh` |
| **Binaries** | [Releases](https://github.com/Fen0l/nutsh/releases): macOS and Linux, x86_64 and arm64, each archive with its SHA-256. Linux comes as `gnu` and as `musl`, a static binary that runs on any distribution. |
| **From source** | `git clone https://github.com/Fen0l/nutsh && cd nutsh && make release`, with a Rust toolchain. The binary lands at `target/release/nutsh`. |

`releases/latest/` skips pre-releases, so the installer URL names the version until there is a
stable one. Not on crates.io yet.

## Usage

The [usage guide](https://nutsh.dev/usage) covers the first ten minutes. The whole key map is in
[docs/keys.md](docs/keys.md). The essentials:

| Key | |
|---|---|
| <kbd>:</kbd> | palette: any kind, page or command |
| <kbd>/</kbd> | filter the rows in front of you |
| <kbd>j</kbd> <kbd>k</kbd> <kbd>g</kbd> <kbd>G</kbd> | move, first, last |
| <kbd>⏎</kbd> | drill into a row's children |
| <kbd>y</kbd> | detail; <kbd>Y</kbd> and <kbd>J</kbd> for raw YAML and JSON; <kbd>w</kbd> watches the row, lighting what changes |
| <kbd>a</kbd> | actions for this row; <kbd>space</kbd> marks rows for a bulk action |
| <kbd>S</kbd> <kbd>w</kbd> | sort by the next column, show twenty columns |
| <kbd>⇥</kbd> <kbd>ctrl-b</kbd> | focus the sidebar, hide it |
| <kbd>ctrl-r</kbd> <kbd>ctrl-t</kbd> | refresh now, change how often this view polls |
| <kbd>?</kbd> | help; <kbd>q</kbd> quits |

A bare `nutsh` opens the **Dashboard**. The sidebar holds seven curated groups, Compute &
Storage through Disaster Recovery, each opening a **page** of panes before the resources under
it, then everything else grouped by API namespace. Nothing appears twice.

The palette knows every kind and these commands:

| | |
|---|---|
| `:ctx [name]` | switch environment, or open the Contexts screen |
| `:search <term>` | name, address or id, across every loaded kind, then the Prism Central |
| `:can-i <action> <kind>` | whether your account may do that, and what it needs |
| `:journal` `:activity` | what this session attempted, and every request it made |
| `:settings` | every setting, its value, where it came from, and the refresh schedule |
| `:skin [name]` | pick a colour scheme |
| `:hide` `:show` | keep things out of the sidebar, or put them back |

Useful from the shell, without entering the TUI:

```sh
nutsh --check            # connect, probe every namespace, print what this PC serves; exit 2 if one is missing
nutsh vm --snapshot      # render one frame to stdout and exit
nutsh --readonly         # refuse every mutation this session
nutsh ctx list           # your contexts; `use`, `show` and `remove` do what they say
NUTSH_LOG=debug nutsh --check   # one run with the log on stderr
```

## Not doing something by accident

Five checks run before a request goes out, and the first one that objects wins: read-only,
your own deny rules, permission, the bulk ceiling, then confirmation. `--readonly`, or
`readonly` on a context or in the config file, refuses every mutation for the session. Beyond
that, guardrails match on context, kind and action:

```toml
[[guardrails]]
contexts = ["prod"]
actions  = ["delete"]
deny     = true
reason   = "deletions in prod go through change control"
```

Anything refused says so in the menu beside it, so nothing is ever offered and then taken away.
Deleting asks you to type the name; powering off asks for a plain yes. A mouse click can never
fire a mutating action or answer a confirmation.

## Configuration

`~/.config/nutsh/config.toml`, written by the app itself when you change something. `:settings`
inside the app lists every setting, its current value, and where that value came from.
Passwords are never in it. The full file, skins, per-kind refresh rates, guardrails, the
sidebar, the cache, the log and request pacing are in [docs/reference.md](docs/reference.md).

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

**One thing to know about the v4 API.** Prism Central's own console still reads several entity
types through the older v3 `groups` endpoint, and those have no populated v4 collection behind
them. So a pane can say `no recovery plans` while the console lists several. **An empty v4
collection means the v4 API has none; it does not prove the Prism Central has none.** Recovery
plans, DR jobs, projects, blueprints and marketplace items are the known cases.
[docs/reference.md](docs/reference.md) has the detail, and why v3 is not implemented.

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
- [CHANGELOG.md](CHANGELOG.md): what changed, release by release

## Contributing

Issues and pull requests are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) has the build, the
tests, and the invariants that stop somebody undoing a fix without knowing what it prevented.
Read it before touching the HTTP client: this program talks to systems with lockout policies.
A security defect goes through [SECURITY.md](SECURITY.md), not an issue.

Built with a lot of AI assistance, and said so plainly: the author wrote the mockups and the
rules about what it must never do, and Claude wrote most of the rest. Every change is reviewed
and adapted by a human before it goes near a lab or a client environment, and the safeguards
against an accidental deletion were designed by that human, not generated. It has run on the
author's own laptop, against the author's own lab, for a few months. The tests and the guard
rails are in the repository, and they are what to judge it on.

## License

MIT. The text is in [LICENSE](LICENSE).

Nutanix, Prism Central and AHV are trademarks of Nutanix, Inc. This project is independent, and
not affiliated with or endorsed by Nutanix.
