# Keys

Every key nutsh answers to, by screen. `?` inside the app shows the short version. Keys are
fixed in this release; the palette commands and the config file are documented in
[reference.md](reference.md).

- [Everywhere](#everywhere)
- [Tables](#tables)
- [Filtering](#filtering)
- [Pages](#pages)
- [The sidebar](#the-sidebar)
- [Detail](#detail)
- [The palette](#the-palette)
- [The action menu](#the-action-menu)
- [Confirmations](#confirmations)
- [Direct action keys](#direct-action-keys)
- [Contexts](#contexts)
- [The mouse](#the-mouse)
- [Palette commands](#palette-commands)

## Everywhere

| Key | Action |
|---|---|
| <kbd>:</kbd> | open the palette: a kind, a page, or a command |
| <kbd>?</kbd> | help overlay; <kbd>esc</kbd>, <kbd>?</kbd> or <kbd>q</kbd> closes it |
| <kbd>esc</kbd> | back, or close whatever is open |
| <kbd>ctrl-b</kbd> | hide or show the sidebar |
| <kbd>1</kbd> to <kbd>9</kbd> | jump to that curated sidebar group, unless a filter term is being typed |
| <kbd>ctrl-o</kbd> | release the mouse for the rest of the session (`:mouse` does it for good) |
| <kbd>q</kbd> | quit; twice while a task is being watched |
| <kbd>ctrl-c</kbd> | quit, always |

## Tables

| Key | Action |
|---|---|
| <kbd>j</kbd> <kbd>k</kbd> or <kbd>↓</kbd> <kbd>↑</kbd> | next row, previous row |
| <kbd>g</kbd> <kbd>G</kbd> or <kbd>Home</kbd> <kbd>End</kbd> | first row, last row |
| <kbd>PgUp</kbd> <kbd>PgDn</kbd> | twenty rows at a time |
| <kbd>←</kbd> <kbd>→</kbd> | scroll columns |
| <kbd>⏎</kbd> | drill into the row's children |
| <kbd>y</kbd> | detail, composed and readable |
| <kbd>Y</kbd> <kbd>J</kbd> | the raw YAML or JSON that came off the wire |
| <kbd>a</kbd> | actions for this row |
| <kbd>space</kbd> | mark a row; <kbd>a</kbd> then acts on every marked row, five at most |
| <kbd>S</kbd> | sort by the next column; again on the same column reverses it |
| <kbd>w</kbd> | wide: twenty columns instead of the curated few |
| <kbd>/</kbd> | filter the rows |
| <kbd>ctrl-r</kbd> | refresh now |
| <kbd>ctrl-x</kbd> | stop polling this view |
| <kbd>ctrl-t</kbd> | how often this view polls |

## Filtering

<kbd>/</kbd> opens a term at the bottom of the table. It matches text in any column and makes
no request.

| Key | Action |
|---|---|
| typing | narrows the rows as you go |
| <kbd>↑</kbd> <kbd>↓</kbd> | move through the rows that match while the term is still open |
| <kbd>⏎</kbd> | keep the filter and hand the keys back to the table |
| <kbd>esc</kbd> | drop the filter |

`:search <term>` is the other kind of search: every loaded kind at once, then the Prism
Central itself. It is a palette command, below.

## Pages

A page is several panes on one screen. The Dashboard and Disaster Recovery are pages.

| Key | Action |
|---|---|
| <kbd>⇥</kbd> <kbd>shift-⇥</kbd> | next pane, previous pane |
| <kbd>O</kbd> | open the focused pane as a full table |
| <kbd>⏎</kbd> or <kbd>y</kbd> | detail of the selected row |
| <kbd>a</kbd> | actions for the selected row |
| <kbd>j</kbd> <kbd>k</kbd> <kbd>g</kbd> <kbd>G</kbd> <kbd>PgUp</kbd> <kbd>PgDn</kbd> | move within the focused pane |
| <kbd>ctrl-r</kbd> <kbd>ctrl-x</kbd> <kbd>ctrl-t</kbd> | refresh, stop, schedule, as in a table |

## The sidebar

| Key | Action |
|---|---|
| <kbd>⇥</kbd> | focus the sidebar from a table; <kbd>⇥</kbd> or <kbd>esc</kbd> again gives focus back |
| <kbd>j</kbd> <kbd>k</kbd> <kbd>g</kbd> <kbd>G</kbd> | move |
| <kbd>h</kbd> <kbd>←</kbd> | fold a group |
| <kbd>l</kbd> <kbd>→</kbd> | unfold a group |
| <kbd>⏎</kbd> | open the selected entry |
| <kbd>/</kbd> | filter the labels while the sidebar has focus |
| <kbd>1</kbd> to <kbd>9</kbd> | jump to the n-th curated group; the digits stop where the curated groups do |

Entries the Prism Central does not serve are greyed, not hidden, and say why. `:hide` and
`:show` change what is listed; `:all` shows everything for the session.

## Detail

| Key | Action |
|---|---|
| <kbd>Y</kbd> <kbd>J</kbd> | switch to the raw YAML or JSON |
| <kbd>w</kbd> | watch this row: polled at the advertised tier's rhythm, a value that changed lit for three seconds; again to stop |
| <kbd>a</kbd> | actions for this resource |
| <kbd>ctrl-t</kbd> | how often the underlying view polls |
| <kbd>esc</kbd> | back to the table |

## The palette

<kbd>:</kbd> opens one ranked list of kinds, pages and commands, completing as you type. The
rest of the best match appears dimmed ahead of the cursor. Arguments complete against real
values: `:ctx ` offers your contexts, `:skin ` your skins. History survives a restart.

| Key | Action |
|---|---|
| <kbd>⇥</kbd> | accept the completion |
| <kbd>↑</kbd> <kbd>↓</kbd> | pick from the list |
| <kbd>ctrl-p</kbd> <kbd>ctrl-n</kbd> | previous, next line from history |
| <kbd>ctrl-w</kbd> | delete a word |
| <kbd>⏎</kbd> | run |
| <kbd>esc</kbd> | cancel |

## The action menu

<kbd>a</kbd> on a row lists what this kind can do, with the key that reaches each action
directly from the table in brackets. What you are not allowed to do is greyed, with the reason.
Below a rule, every remaining operation the API declares, by its raw name.

| Key | Action |
|---|---|
| typing | filter the list |
| <kbd>↑</kbd> <kbd>↓</kbd> | move |
| <kbd>⏎</kbd> | run the selected action |
| <kbd>esc</kbd> | close |

## Confirmations

| Prompt | Keys |
|---|---|
| yes or no | <kbd>y</kbd> runs, anything else cancels |
| type the name | type the resource's name exactly, then <kbd>⏎</kbd>; <kbd>esc</kbd> cancels |
| a form | <kbd>⇥</kbd> <kbd>shift-⇥</kbd> between fields, <kbd>space</kbd> toggles, <kbd>⏎</kbd> submits, <kbd>esc</kbd> cancels |

Confirmations are keyboard-only. A mouse click can never answer one.

## Direct action keys

These run from the table without opening the menu. The confirmation is a floor set by the
action's danger in the catalog: low asks nothing, medium asks yes or no, high asks for the
name to be typed. A `[[guardrails]]` rule in the config file can raise it, never lower it.

| Key | Action | On | Confirmation |
|---|---|---|---|
| <kbd>p</kbd> | Power on | VM | none |
| <kbd>P</kbd> | Power off (cuts power) | VM | yes or no |
| <kbd>d</kbd> | Shut down (ACPI, asks the guest) | VM | yes or no |
| <kbd>D</kbd> | Shut down (guest tools) | VM | yes or no |
| <kbd>r</kbd> | Reboot (ACPI, asks the guest) | VM | yes or no |
| <kbd>R</kbd> | Reboot (guest tools) | VM | yes or no |
| <kbd>m</kbd> | Migrate to host | VM | yes or no, after a form: the host |
| <kbd>C</kbd> | Clone | VM | none, after a form: the new name |
| <kbd>s</kbd> | Create recovery point | VM | none, after a form: name and expiry, thirty days by default |
| <kbd>ctrl-d</kbd> | Delete | VM | type the name |
| <kbd>M</kbd> | Enter host maintenance | Host | yes or no |
| <kbd>c</kbd> | Cancel | Task | yes or no |
| <kbd>A</kbd> | Acknowledge | Alert | none |
| <kbd>R</kbd> | Resolve | Alert | none |

Power cycle, reset, revert and migrate to another cluster are in the menu without a direct key.
Every `delete` on every kind asks for the name to be typed, and the power verbs, revert and
entering host maintenance always ask yes or no, whatever the catalog says. A read-only session
refuses all of them before anything reaches the wire.

## Contexts

`:ctx` opens the Contexts screen, which is also what a start with no usable context shows.

| Key | Action |
|---|---|
| <kbd>⏎</kbd> | connect to the selected context |
| <kbd>a</kbd> | add a context |
| <kbd>l</kbd> | log in: asks for the password once, checks it, stores it |
| <kbd>d</kbd> | remove a context |
| <kbd>ctrl-r</kbd> | reload the config file |
| <kbd>q</kbd> | quit |

The add form takes name, host, port, username, password, an optional cluster, and the
insecure and read-only switches. <kbd>⇥</kbd> moves between fields, <kbd>space</kbd> toggles a
switch, <kbd>⏎</kbd> submits.

## The mouse

Clicking works on rows, menu entries, column headers and the version line in the header, which
opens `:settings`. The wheel scrolls the pane under the pointer. Shift-drag still selects text
the way your terminal expects. A click can never fire a mutating action or answer a
confirmation; those are keyboard-only, deliberately. <kbd>ctrl-o</kbd> releases the mouse for
the session; `:mouse` turns capture off and writes that to the config file.

## Palette commands

| Command | What it does |
|---|---|
| `:ctx [name]` | the Contexts screen, or straight to a context |
| `:search <term>` | name, address or identifier across every loaded kind, then the Prism Central |
| `:can-i <action> <kind>` | whether your roles permit that action on that kind, and what it needs |
| `:try <kind>` | ask this Prism Central for a kind the catalog says its API version does not have |
| `:journal` | every action attempted this session, and what answered; never written to disk |
| `:export [csv\|json] [path]` | the table in view as it is drawn, to a file; bare, a stamped csv in the working directory |
| `:activity` | every request this session made, and every table it holds; <kbd>⇥</kbd> switches tab |
| `:settings` | every setting, its value, where it came from, and the refresh schedule per namespace and kind |
| `:refresh <interval>` | how often the current view polls: seconds, `auto`, or `off` |
| `:skin [name]` | the skin picker, or straight to a skin; written to the config file |
| `:header` | `auto`, `compact` or `full`; written to the config file |
| `:mouse` | mouse capture on or off; written to the config file |
| `:log [level]` | logging on at `debug` or off again; a level names one exactly |
| `:hide <id or group>` | drop something from the sidebar; written to the config file |
| `:show <id>` | put it back |
| `:all` | show everything for this session, including what the Prism Central does not serve |
| `:help` | the help overlay |
| `:quit` | leave |
