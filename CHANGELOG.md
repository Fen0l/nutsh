# Changelog

## Unreleased

### Fixed

- A 401 from one endpoint on a session that still works elsewhere no longer costs a password.
  The session is checked on a URL it already answered before any renewal; if it still answers,
  the 401 is that endpoint's verdict for the account - the table says `not permitted for this
  account (HTTP 401)` and everything else carries on. Before this, one such endpoint (IAM
  `users` on one Prism Central) made every run present the password a second time and then
  report the credential refused. Nothing changes for a session that really ended, or a
  password that really is wrong: one presentation, then stop.
- `:export` writes the file `0600` and refuses to write through a symlink.

## 0.0.2-beta3

Still a beta. This one is about seeing what the session is doing and getting to the answer
faster: a request log, a watch on a row, export, completion, and two pages that put the
evidence side by side.

### Session

- `:activity` shows what this session is doing: the last 500 requests with method, path,
  status, round trip and which credential they used, and on a second tab (`⇥`) every table the
  session holds with its row count, its source and when it last polled. A request refused
  before the wire is a row too, so a poller that went quiet is visible.
- `w` on a detail pane watches the row: polled at a quarter of the tier the Prism Central
  advertises, never faster than once a second, and a value that changed is lit for three
  seconds. `w` again stops; `esc` closes the pane and the watch with it.

### Getting the data out

- `:export [csv|json] [path]` writes the table in view as it is drawn: same columns, same
  order, the `/` filter respected, names not ids. Bare `:export` is a csv in the working
  directory, stamped to the second.
- `nutsh vm --snapshot --format csv|json` prints the same to stdout, for scripts.

### Pages

- Attention: unresolved critical and warning alerts, failed tasks, powered-off VMs and the
  hosts, four panes side by side with a summary that counts them and the protected VMs out
  of sync. A source namespace the Prism Central does not serve says so on its pane.
- A failed task's detail gathers the alerts raised ten minutes either side of its start, and
  the journal entry if this session launched it - `not started from here` when it did not.

### Shell

- `nutsh completions zsh|bash|fish|elvish|powershell` prints the completion script.
  `[KIND]` completes from the catalog - ids, aliases, pages - and `-c` and
  `cache clear --only` from the context names in the config file. The secret store is never
  opened for it.

### Settings

- The version line in the header opens `:settings` on a click.
- `⏎` on a refresh row shows the whole ladder; `space` still steps one rung.
- A kind with no open table shows `never` as its last refresh, not `-`.

### Smaller

- README rewritten: demo, feature list, quick start. `docs/keys.md` is new and lists every key,
  every palette command and every direct action key.

## 0.0.2-beta2

Still a beta. Search now asks the Prism Central, refresh can be set per namespace and per kind,
and two namespaces the first beta did not show are in.

### Search

- `:search <term>` answers from what is loaded, then asks every kind that can be filtered by
  name. Results fill in as they land, and the footer says how many kinds were asked and how many
  answered.
- A full identifier is searched as an exact match. An address search covers the loaded rows
  plus the nine kinds that keep an address.
- Search results never replace a table you have open.
- Kinds this Prism Central has already refused are not asked again.

### Settings

- `:settings` has a refresh section grouped by namespace. Each namespace and each kind can carry
  its own schedule, kept as `[refresh.namespaces]` and `[refresh.kinds]`, and the screen shows
  where each value came from and when it last polled.
- The ladder reaches `1h`, `6h` and `12h`. `:refresh 6h` is accepted.
- `refresh everything now` sits at the top of the section.
- `cache_max_age` in the config file says how old a cached inventory may be at start. The
  default is seven days.

### Objects and Files

- Two new menu sections. Objects: object stores, each with its buckets, certificates and
  cluster. Files: file servers, shares, snapshots, replication policies and jobs.
- Federated object namespaces are not listed. The v4 API has no call for them.

### Logging

- There is a log now, off by default. Turn it on with `NUTSH_LOG` in the environment, `log` in
  the config file, or `:log` in the app, in that order of precedence.
- `NUTSH_LOG` takes a level or a `tracing_subscriber` filter string.
- One-shot commands log to stderr. The TUI and `--snapshot` log to
  `$XDG_STATE_HOME/nutsh/logs/nutsh.log`, capped at 4 MB with one rollover.
- Every request is one line at `debug`: method, URL, status, elapsed time and which credential
  it went out with. A 401 or a 5xx is a `warn`.
- No credential ever reaches the log: no headers, no password, no cookies, no request body.
  `tests/log.rs` checks that at the highest verbosity.

### Smaller

- Derived column headings drop schema noise: `HOST EXT ID` reads `HOST`.
- The README carries a generated v4 coverage table, with the per-namespace detail in
  `docs/coverage.md`. `make ci` fails if it drifts from the catalog.
- The two largest files in `crates/tui` were split. No behaviour change.

## 0.0.2-beta1

The first build worth handing to somebody else. A terminal client for Nutanix Prism Central over
the v4 API: tables and pages you move through with the keyboard, and a reason on screen whenever
something is missing.

A beta because one person has used it, against one Prism Central and a mock built from the
published specs.

### Worth knowing first

- Requests are paced from the rate limit the Prism Central advertises, not from a constant.
  A rejected credential is never presented twice.
- The first connect takes about six seconds: twenty version probes, paced. They are cached, so
  later runs open at once.
- Upgrading discards the cache once, silently. The next run negotiates again.

### Look

- Seventeen skins. `:skin` picks one and writes it to the config file.
- Truecolour, 256 colours or sixteen, detected.
- A `[skin]` section: name, background on or off, per-role colour overrides.

### Navigation

- Curated groups in the sidebar, `1` to `9` to jump to one. Below them, everything else grouped
  by API namespace. Nothing is listed twice.
- A kind this Prism Central does not serve is dimmed, and `enter` on it says why.
- `[nav] hide`, `:hide`, `:show` and `:all` decide what is listed. `hide_unserved` drops the
  unserved kinds instead of dimming them.
- `esc` goes back to the menu.

### Pages

- `nutsh` opens the Dashboard. Each big group opens on a page of panes; `O` opens a pane as a
  full table.
- A pane whose namespace is missing says so. The rest of the page still draws.

### Actions and guardrails

- `a` opens the actions for a row: curated verbs first, raw API names below a rule.
- Verbs say what they do: `Power off (cuts power)`, `Shut down (ACPI, asks the guest)`.
  Lower-case keys go through ACPI, upper-case through the guest tools.
- `space` marks rows, and `a` then acts on all of them.
- Dangerous actions confirm. Delete asks for the name.
- `[[guardrails]]` rules deny an action, force a confirmation or cap a bulk run, by context,
  kind and action.
- `--readonly`, or `readonly = true` in the config file, refuses every mutation.
- Permissions come from the IAM policies. An action the account cannot run is greyed, with
  the roles it would need.
- `:journal` lists every attempt and what answered. `:can-i <action> <kind>` asks without
  doing anything.
- A started task is watched to its end, with progress in the status line.

### Readability

- Names instead of UUIDs, resolved through a name cache.
- Curated columns for the curated kinds, schema-derived columns for the rest. `w` shows twenty.
- `y` opens a composed detail, `Y` the raw YAML, `J` the raw JSON. Empty sections are not drawn.
- Bytes, timestamps, percentages and addresses are formatted as such.
- Large collections list newest first within a row budget, and the title says how much is
  shown. `ctrl-x` stops a long walk and keeps what already arrived.

### Cache and requests

- Rows, negotiated versions and names are cached per context and painted on the first frame,
  marked `◌ cached` until live data replaces them.
- `cache = true` by default. `--no-cache` for one run. `nutsh cache info` and `cache clear`.
- Passwords never reach the cache, the config file, argv, a child process, the journal or a
  fixture.
- Lists ask only for the columns they draw, which takes 68% off a VM list and 84% off alerts.
- Where a kind has a monotone timestamp, a one-row probe replaces a full walk.
- Pollers stop after five idle minutes and restart on a keystroke.
- `ctrl-t` sets how often the current view polls. `:refresh` writes it to `[refresh]`.
- The status line shows requests per second and the cache share: amber when paced, red when
  refused.

### Palette

- `:` opens one ranked list of kinds, pages and commands. Ghost text completes the best match;
  `⇥` accepts it.
- Arguments complete from real values: contexts, skins, hidden items, actions, kinds.
- `ctrl-p` and `ctrl-n` walk the history, stored beside the cache. Long lines and anything
  shaped like a secret are never recorded.

### Settings, search and the mouse

- `:settings` lists every setting with its value and where it came from. `space` toggles a
  switch and writes it back.
- `/` filters the rows in front of you. `:search <term>` searches every loaded kind and says
  how far it looked.
- The mouse is on by default: rows, menu items, headers, the wheel. `ctrl-o` releases it for
  the session, `:mouse` for good. A click never fires a mutation or answers a confirmation.
- `:header` cycles `auto`, `compact` and `full`.
- `nutsh ctx add|login|use|list|show|remove` manage contexts. The Contexts screen does the same
  inside the app.
- `--check` prints what this Prism Central serves and at which version. `--snapshot` prints one
  frame and exits.

### Known limitations

- Tested against one Prism Central at pc.7.6 and a mock.
- No prebuilt binaries yet. `make release` builds one.
- No server-side filtering. `/` and `:search` work on the rows already loaded.
