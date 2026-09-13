# Changelog

## Unreleased

### Logging

- `tracing` had seventeen call sites in this workspace and no subscriber, so every one of them
  was discarded. There is a subscriber now, and a switch for it: `NUTSH_LOG` in the environment,
  `log` in the config file, `:log` inside the app, in that order of precedence. The default is
  `off`, and a run nobody asked anything of leaves no file behind.
- `NUTSH_LOG` takes a level word or a `tracing_subscriber` filter string, so
  `NUTSH_LOG=nutsh_prism=debug` asks for the requests and nothing else. A word that is neither
  says so rather than installing silence.
- Where the lines go is a property of the run. `--check`, `--info`, `ctx` and `cache` write to
  stderr; the TUI and `--snapshot` cannot, because a line on stderr is a hole in the frame, and
  write to `$XDG_STATE_HOME/nutsh/logs/nutsh.log` instead. The file is capped at 4 MB and rolls
  over one copy beside it, so an overnight session costs 8 MB and keeps the recent end.
- Every request is one line at `debug`, from the one funnel every request goes through: the
  method, the URL, the status, the elapsed time, and which credential it went out with -
  `presented`, `session`, or `refused` for one this program stopped before the network. A 401 or
  a 5xx is a `warn`, so finding one does not mean reading at `debug`. Which request got the 401
  is now a question with an answer, and so is which kind of 401 it was: one on a request that
  rode a session is that session ending, one on the request that then presented the password is
  the password being refused.
- A log never carries a credential: not the `Authorization` header, not the password, not a
  `Cookie` or `Set-Cookie` value, not a request body. A session cookie is credential-equivalent
  for about fifteen minutes, so it is not written even truncated. No level and no filter string
  can turn on a dependency that would produce one - `hyper` and `h2` log frames, and a frame is
  a header dump - because only this program's own crates reach the writer. `tests/log.rs` drives
  a session at the highest verbosity there is and asserts that the exact values that went over
  the wire appear nowhere in what came out.

## 0.0.2-beta1

The first build worth handing to somebody else, and a beta. nutsh is a terminal client for
Nutanix Prism Central: it reads the v4 APIs, draws them as tables and pages you can move around
in, and tells you why something is missing instead of showing you an empty box.

It is a beta because none of it has been through a week of ordinary use by anybody but its
author, and because the only Prism Centrals it has met are one installation and a mock built from the
published specs. Everything below is implemented and tested; what has not happened yet is
somebody else's afternoon with it. Expect to find rough edges, and expect the version number to
move.

### Three things worth knowing before anything else

**Requests are paced by the limit the Prism Central advertises.** Every response carries
`x-api-ratelimit-limit` and its refresh period, and the token bucket is driven from those rather
than from a number compiled into the program. The old constant was thirty requests a second
against a Prism Central that allows three. A rejected credential is now terminal everywhere: it
is never presented a second time, the pollers stop, and the rows already on screen stay where
they are.

**The first connect takes about six seconds** against a rate-limited Prism Central, because
twenty version probes are paced honestly instead of fired at once. The pins are cached, so a
later run issues none of them and opens immediately. Only a first connect, or a cache that has
expired, pays it.

**Upgrading discards the cached contexts once.** The cache records the version that wrote it and
refuses a directory written by a different one, so the first run after an upgrade negotiates
again and writes a fresh cache. It is silent and it costs one connect.

### The look, and skins

- Seventeen skins, with `:skin` to change one and the palette to complete its name. The choice
  is written back to the config file.
- Colour depth is detected: truecolour, 256 colours, or sixteen.
- A `[skin]` section takes a name, whether to paint the background, and per-role colour
  overrides.
- Semantic roles rather than literal colours, so a status word is the same colour in a table
  cell, a detail field and the confirm dialog.

### Navigation and the sidebar

- A menu of curated groups on the left, with `1` to `9` jumping to one and the digit keys
  stopping where the curated groups do.
- Beneath them, under a `─── by namespace ───` rule, every kind the curated menu left over,
  grouped by API namespace. Nothing is listed twice.
- A kind this Prism Central does not serve is drawn dim, and `enter` on it says why rather than
  opening an empty table.
- Nothing is hidden unless it was asked for: `[nav] hide` is a list you write, `:hide` and
  `:show` edit it from inside the app, and `:show ` completes from what is hidden, which is the
  only place a group hidden whole still has a name. `[nav] hide_unserved` drops the unserved
  kinds instead of greying them, and `:all` suspends that rule for the session.
- `esc` goes back to the menu.

### Pages and panes

- A bare `nutsh` opens the Dashboard.
- Each big group opens on its own landing page first: Compute & Storage, Network & Security,
  Hardware, Monitoring, Policies and Admin Center, beside the Dashboard and Disaster Recovery.
  The resources are still underneath.
- A pane fetches the rows it can show rather than the kind's whole budget, and `O` opens the
  pane's kind as a full table.
- A pane whose namespace is missing says so in its own body; the rest of the page still draws.

### Actions, guardrails and the journal

- `a` opens the actions for the row under the cursor, and the same list appears in the `⏎`
  picker and as an `ACTIONS` section of the detail pane. One list, built and greyed once, so
  the three can never disagree.
- Verbs say what they do: `Power off (cuts power)`, `Shut down (ACPI, asks the guest)`,
  `Reboot (guest tools)`. Lower-case keys go through ACPI, upper-case through the guest tools.
- A line is drawn under the curated verbs; below it are the raw API names nobody has curated.
- `space` marks rows and `a` then acts on every marked one, as a single ordered run.
- Dangerous actions confirm, and the most dangerous ask for the entity's name.
- `[[guardrails]]` rules deny an action, force a confirmation, or cap how many rows one run may
  touch, scoped by context, kind and action.
- `readonly = true` in the file or `--readonly` on the command line refuses every mutation for
  the session.
- Permissions are resolved from the IAM authorisation policies, so an action the account cannot
  perform is greyed with the roles it would need, not offered and then refused by the server.
- Every attempt goes in the journal, which `:journal` shows: what was asked, what answered, and
  the upstream text if there was any.
- A started task is watched to its end, with its progress on the status line.
- `:can-i <action> <kind>` answers the same question without touching anything.

### Readability

- Names, not UUIDs. A reference is resolved through a name cache warmed at connect and fed by
  every poll, so a cluster reads `prod-01` rather than thirty-six hex characters.
- Curated columns for the kinds worth curating, derived columns from the schema for the rest,
  and `w` widens to twenty.
- `y` opens a composed detail: sections curated per kind, or grouped from the document itself
  for the kinds nobody curated. `Y` and `J` show the wire YAML and JSON unchanged.
- A section every one of whose fields is empty is not drawn, so a Prism Central that does not
  return something is not represented by a column of dashes.
- Cells are human: bytes as `8 GiB`, microsecond timestamps as `31d`, percentages as `100%`,
  IP addresses recognised as addresses, a nil UUID as a dash.
- A table walking a large collection says so in its title (`[0/181825] [↻ 100 rows · 1s]`) and
  in its body (`listing up to 500 rows…`), from the first frame, before any response.
- Every kind has a row budget, and the big ones are ordered newest first, so the rows a budget
  keeps are the rows worth keeping. A Prism Central can hold 181 825 audit records; the table shows the
  newest five hundred and says that is what it is showing.
- `ctrl-x` ends a walk that is taking too long and keeps every page it already staged: they are
  paid for, and throwing them away would make the keystroke a punishment. The header reads
  `⏹ stopped`, the count is the honest one, and `ctrl-r` starts the walk again.

### Caching and requests

- The pins, the rows and the resolved names are cached per context, read before connecting and
  painted on the first frame, then replaced as the live cycles land. A restored table says
  `◌ cached 12m` until it is.
- `cache = true` is the default, `--no-cache` turns it off for one run, and
  `nutsh cache info|clear` shows and removes what is on disk.
- Passwords never reach the cache, the config file, argv, a child's environment, the journal or
  a fixture. A secret-shaped field is deleted on the way to disk.
- Lists ask for the columns they draw. `$select` is generated per kind as the union of every
  property a table row is actually read for, and it takes 68% off a virtual machine list and
  84% off alerts. A Prism Central that refuses one is asked for the whole document instead,
  once, and remembered.
- A change probe replaces a walk where a kind has a monotone timestamp: one `$limit=1` request
  instead of five pages, against the probe's own previous answer.
- Five idle minutes stop every poller nobody is reading. A keystroke starts them again.
- `ctrl-t` sets how often the view in front of you polls, for this session: `auto`, `5s`, `10s`,
  `30s`, `1m`, `5m`, `off`. `:refresh` keeps the same value in `[refresh]`. Precedence is one
  function: the session's override, then `[refresh.kinds]`, then `[refresh] default`, then the
  catalog's own rhythm, and `auto` names the catalog at every level. A kind with no schedule
  reads `⏹ manual`, and `ctrl-r` there runs exactly one cycle.
- The status line carries a request meter: requests a second, and how much of the last minute
  came from the cache. It turns amber when requests are being paced and red when they are being
  refused.

### Completion and the palette

- `:` opens one ranked list of kinds, pages and commands. Commands are `ctx`, `quit`, `help`,
  `skin`, `journal`, `search`, `can-i`, `mouse`, `header`, `settings`, `refresh`, `all`, `hide`
  and `show`.
- Ghost text completes the rest of the best match in dim, after the cursor; `⇥` accepts it.
- Arguments complete from the right vocabulary, and the popup says what it is completing:
  contexts after `:ctx`, skins after `:skin`, menu items after `:hide`, hidden items after
  `:show`, actions and kinds after `:can-i`.
- A command that takes one argument refuses two.
- `ctrl-p` and `ctrl-n` walk the lines this context has accepted, stashing whatever was being
  typed on the first press and giving it back past the newest. The history lives beside the
  cache, under the same switch: `cache = false` or `--no-cache` and nothing is written down. A
  line over 120 bytes or shaped like key material is never recorded, because `:ctx` takes free
  text and a password pasted into the wrong window must not become a file.

### Settings, search and the mouse

- `:settings`, and a `Settings` item in the menu, list every setting this build has with its
  value and where that value came from: a default, the config file, a context, or the flag this
  run was started with. `space` writes a switch back to the file; the `[nav] hide` list is on
  the same screen with `a` to add an entry and `d` to remove one. A setting the screen cannot
  honestly change is shown with its reason rather than left out: read-only and the guardrails
  are both compiled into the session at connect. A setting that is on and not listed is one you
  hunt for in the wrong file.
- `/` filters the rows in front of you on name, IP or identifier, and the title says so:
  `Virtual Machines [12 of 847] /web`.
- `:search <term>` reaches further: every kind already loaded or restored from the cache,
  grouped by kind. It asks for nothing, and the footer says exactly how far it reached -
  `searched 14 loaded kinds · 248 not loaded`. That sentence is the point of the feature.
- The mouse is on by default: click a row, a menu item or a pane's header, and the wheel
  scrolls whatever is under it. Shift-drag still selects text, `ctrl-o` turns capture off for
  the session, and `:mouse` turns it off for good. A click can never fire a mutating action or
  answer a confirm dialog.
- `:header` cycles the header between `auto`, `compact` and `full`, and writes the choice down.
  Below twenty-eight rows `auto` folds the seven-line box to one line that keeps the context,
  the cluster scope, read-only, the freshness and the breadcrumb.
- `nutsh ctx add`, `login`, `use`, `list`, `show` and `remove` manage named Prism Centrals, and
  the Contexts screen does the same from inside the app.
- `--check` connects, probes every API namespace and prints a table of what this Prism Central
  serves and at which version.
- `--snapshot --size COLSxROWS` renders one frame to stdout and exits.

### Known limitations

It has not been run against a wide range of Prism Centrals. One installation at pc.7.6 and a mock built
from the published specs are the whole of what has answered it, so a deployment that disagrees
with its own spec file is the likeliest thing to break.

Deployment is deliberately out of scope: there are no prebuilt binaries, no Homebrew tap and
nothing published to crates.io. Build it with `make release`, which prints the path and the byte
count of what it made.

Server-side filtering is not used. `/` and `:search` work over the rows this session has
loaded, and they say so rather than implying they searched an inventory they never asked for.
