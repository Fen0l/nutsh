# Changelog

nutsh is a terminal client for Nutanix Prism Central over the v4 API: every resource kind
behind one prompt, names instead of UUIDs, and a guardrail in front of anything that changes
state. This file says what each release added, fixed or changed, newest first.

Sections: ✨ new · 🐛 fixed · 🔒 security · ⚡ performance · 🎨 look · 📚 docs · 📦 release ·
⚠️ known limitations.

## Unreleased

### ✨ nutsh demo

- `nutsh demo` opens the ordinary screens on a built-in Prism Central: an invented estate,
  every page populated, no config file and no credential needed, and nothing of yours written.
  `nutsh demo vm`, `--snapshot`, `--readonly` work as they do on a real one; the Contexts
  screen offers `demo` and `demo-edge`, the same site under a cluster pin with guardrails of
  its own. The empty Contexts screen now suggests it; two built-in Prism Centrals, `--join`
  for both in one table, `--site demo-dr` for the second one and its operator account. The
  estate is fuller than a real v4 API: kinds the console reads through v3 `groups` (recovery
  plans among them) have rows in the demo and none on a real Prism Central; the reference
  lists them.

## 0.1.0 - 2026-09-19

**The first release meant for other people.** A few weeks of daily use on my own Prism Central,
three betas, and everything below. Built for my own day-to-day, with a lot of help from an LLM,
and I would be happy to hear from you. It ran fine on pc.7.5 and runs better on 7.6; if you have
an older version, tell me how it went.

### ✨ Several Prism Centrals in one table

- `:ctx lab dr` reads two Prism Centrals side by side; `-c lab,dr` does it from the shell
- Works for the TUI, `--snapshot` and `--check`, which prints one table per context
- CONTEXT column after the name, one colour per Prism Central, header reads `lab +dr`
- `space` on the Contexts screen joins or drops a context; a JOINED column shows which
- `/dr` keeps one Prism Central's rows
- A row acts against its own Prism Central; the confirm and the journal say which
- `y` on a peer's row reads and polls that peer; the pane is titled `on dr`
- Each context keeps its own session, pacing and rate ceiling
- A credential one of them refuses stops that one alone
- A peer's references resolve to names through the shared cache

### 🐛 Fixed

- A 401 from one endpoint on a live session no longer spends a password: the session is
  checked first, and the table says `not permitted for this account`
- `:export` writes `0600` and refuses to write through a symlink
- A picker cancelled from a row leaves no subscription behind

### 📚 Docs

- README rewritten for a first release: status, what it stores, install, first five minutes
- `docs/keys.md` completed: settings, palette, detail and contexts keys

### 📦 Release

- Release archives stripped of symbols
- Installer URL on `releases/latest`

### ⚠️ Known limitations

- Pages, `:search` and the header counters stay the session's own when peers are joined
- Tested on pc.7.5, pc.7.6 and a mock built from the pc.2024.3 specs
- No Windows build; WSL works. Not on crates.io

## 0.0.2-beta3 - 2026-09-15

**Seeing what the session does, and getting to the answer faster.**

### ✨ New

- `:activity`: the last 500 requests with method, path, status, round trip and credential;
  a second tab lists every table the session holds
- `w` on a detail pane watches the row and lights a value that changed
- `:export [csv|json] [path]` writes the table in view as drawn
- `--snapshot --format csv|json` prints the same to stdout, for scripts
- Attention page: unresolved alerts, failed tasks, powered-off VMs and hosts side by side
- A failed task's detail shows the alerts around it and the journal entry
- `nutsh completions zsh|bash|fish|elvish|powershell`; kinds, contexts and cache targets
  complete without opening the secret store
- The version line in the header opens `:settings` on a click
- `⏎` on a refresh row shows the whole ladder

### 📚 Docs

- README with demo, feature list and quick start
- `docs/keys.md`: every key, palette command and direct action key

## 0.0.2-beta2 - 2026-09-15

**Search asks the Prism Central, refresh per namespace and kind, two more namespaces.**

### ✨ New

- `:search <term>` answers from what is loaded, then asks every kind filterable by name;
  exact match on a full identifier; addresses searched across the kinds that keep one
- `:settings` refresh section: a schedule per namespace and per kind, kept in the config file
- Refresh ladder up to `12h`; `:refresh 6h` accepted; `refresh everything now`
- `cache_max_age`: how old a cached inventory may be at start, seven days by default
- Objects section: object stores, buckets, certificates
- Files section: file servers, shares, snapshots, replication policies and jobs
- Logging, off by default: `NUTSH_LOG`, `log` in the config file, or `:log`; one line per
  request at `debug`; the TUI logs to `$XDG_STATE_HOME/nutsh/logs/`, 4 MB with one rollover

### 🔒 Security

- No credential ever reaches the log: no headers, password, cookies or request body, checked
  by a test at the highest verbosity

### 🎨 Look

- Derived column headings drop schema noise: `HOST EXT ID` reads `HOST`

### 📚 Docs

- Generated v4 coverage table in the README, per-namespace detail in `docs/coverage.md`

## 0.0.2-beta1 - 2026-09-13

**The first build worth handing to somebody else.**

### 🔒 Security and pacing

- Requests paced from the rate limit the Prism Central advertises, never from a constant
- A rejected credential is presented once, then everything stops
- Passwords never reach the cache, the config file, argv, a child process or the journal
- `--readonly`, or `readonly = true` in the config file, refuses every mutation
- `[[guardrails]]` rules deny, force a confirmation or cap a bulk run, by context, kind and action
- A click never fires a mutation or answers a confirmation

### ✨ Navigation

- Sidebar of curated groups, `1` to `9` to jump; everything else grouped by namespace
- A kind this Prism Central does not serve is dimmed, with the reason on `enter`
- `:hide`, `:show`, `:all` and `[nav] hide` decide what is listed
- Palette `:` with every kind, page and command; ghost completion, argument completion, history
- `⏎` drills into a row's children, `esc` goes back, `O` opens a pane as a full table
- Dashboard, and a page of panes per group; a missing namespace says so on its pane
- `/` filters the rows in view; `:search` across every loaded kind

### ✨ Actions

- `a` action menu: curated verbs that say what they do, raw API names below a rule
- `space` marks rows for a bulk action
- Dangerous actions confirm; delete asks for the name
- Permissions from IAM: an action the account cannot run is greyed with the roles it needs;
  `:can-i <action> <kind>` asks without doing anything
- `:journal` of every attempt and what answered
- A started task is watched to its end, with progress in the status line

### ✨ Readability

- Names instead of UUIDs, through a name cache
- Curated columns for the curated kinds, schema-derived for the rest; `w` shows twenty
- `y` composed detail, `Y` raw YAML, `J` raw JSON
- Bytes, timestamps, percentages and addresses formatted
- Large collections newest first within a budget; `ctrl-x` stops a walk and keeps the rows

### ⚡ Cache and requests

- Rows, negotiated versions and names cached per context and painted on the first frame,
  marked `◌ cached` until live data replaces them
- `--no-cache`, `nutsh cache info`, `nutsh cache clear`
- Lists ask only for the columns they draw; one-row probes where a timestamp allows
- Pollers pause after five idle minutes
- `ctrl-t` and `:refresh` set how often the current view polls
- Status line shows requests per second and the cache share

### 🎨 Look and input

- Seventeen skins, `:skin`; truecolour, 256 or sixteen colours, detected; `[skin]` overrides
- `:header auto|compact|full`
- Mouse on by default; `ctrl-o` releases it for the session, `:mouse` for good
- `:settings` lists every setting, its value and where it came from

### 📦 Shell

- `nutsh ctx add|login|use|list|show|remove` manage contexts; the Contexts screen does the same
- `--check` prints what this Prism Central serves and at which version
- `--snapshot` prints one frame and exits
