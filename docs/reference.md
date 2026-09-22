# Reference

The detail behind [README.md](../README.md): the whole config file, the cache, the log, how
requests are paced, and what the v4 API does not cover.

## The config file

`$XDG_CONFIG_HOME/nutsh/config.toml`, or `~/.config/nutsh/config.toml`. `NUTSH_CONFIG` points
somewhere else. Passwords are never in it.

```toml
current_context = "lab"
readonly        = false   # refuse every mutation, in every context
cache           = true    # read and write the on-disk cache
mouse           = true    # capture the mouse
header          = "auto"  # auto | compact | full
log             = "off"   # off | error | warn | info | debug | trace

[skin]
name       = "catppuccin-mocha"
background = true
colors     = { peach = "#ffa07a" }   # per-role overrides, by swatch name

[nav]
hide_unserved = false                # drop what this PC does not serve, rather than greying it
hide          = ["Hardware", "iam.authn.DirectoryService"]

cache_max_age = 3600     # seconds a cached inventory may be old at start; default seven days

[refresh]
default = 30             # seconds, or "auto" for each kind's own rhythm, or "off"

[refresh.namespaces]
lifecycle = 3600         # every kind of the namespace, unless one below says otherwise

[refresh.kinds]
"prism.config.Task" = "auto"                  # this one keeps its curated three seconds
"monitoring.serviceability.Audit" = "off"     # and this one polls only on ctrl-r

[[guardrails]]
contexts = ["prod"]
kinds    = ["vmm.ahv.config.Vm"]
actions  = ["delete", "power-off"]
confirm  = "type-name"   # none | yes | type-name
max_bulk = 5

[[guardrails]]
contexts = ["prod"]
actions  = ["delete"]
deny     = true
reason   = "deletions in prod go through change control"

[contexts.lab]
host       = "pc.lab.example"
port       = 9440
username   = "admin"
verify_tls = false
cluster    = "prod-01"

[contexts.prod]
host       = "pc.prod.example"
port       = 9440
username   = "svc-nutsh"
verify_tls = true
ca_bundle  = "/etc/ssl/certs/internal-ca.pem"
readonly   = true        # this context only
```

`readonly` at the top level, `readonly` on one context, and `--readonly` all refuse every
mutation for the session, and any one of them is enough. A guardrail is narrower: it
matches on context, kind and action, and either denies outright with a reason, forces a
confirmation, or caps how many rows one run may touch. Whatever refuses an action says so in
the menu beside it, so nothing is offered and then taken away.

## The cache

The negotiated API versions, the rows and the resolved names are cached per context under
`$XDG_STATE_HOME/nutsh/`, read before connecting and painted on the first frame. A table showing
cached rows says `◌ cached 12m` until a live cycle replaces them.

```sh
nutsh cache info      # what is cached, per context
nutsh cache clear     # the whole tree; --only <context> for one
nutsh --no-cache      # neither read nor write, for one run
```

The cache records the version that wrote it and refuses a directory written by a different one,
so the first run after an upgrade negotiates again. It is silent and it costs one connect.

The palette's line history lives in the same directory and under the same switch, so
`--no-cache` writes none of it and `nutsh cache clear` removes it with everything else.
Discarding a stale cache is not the same act: that leaves the history where it is.

Secrets never reach it: a field whose name looks like a secret is deleted on the way to disk.

## The demo

`nutsh demo` is the program on Prism Centrals it brought with it. What boots: two mock Prism
Centrals, in this process, each on a loopback port picked by the operating system, speaking
plain HTTP; the first serves an invented estate of two clusters, four hosts and twelve VMs with
the alerts, tasks, policies, subnets and users around them. Each reports itself as `pc.7.6`, and the header
shows the real endpoint: `PC: 127.0.0.1:<port>`. Every screen, table, detail, picker and action
menu is the ordinary one; `nutsh demo vm`, `nutsh demo --snapshot`, `--size`, `--format` and
`--readonly` mean what they mean on a real Prism Central. Connection flags and `NUTSH_*`
variables are parsed and ignored.

Two Prism Centrals boot: `demo` (context `demo`, account `admin`; `demo-edge` is the same one
under a cluster pin) and `demo-dr` (account `operator`, who may power VMs on and off and
nothing more). `nutsh demo --join` reads the other one beside the session from the first frame;
`nutsh demo --site demo-dr` starts on the second. The second site shows the three ways a kind
is greyed: a namespace this account may not read (licensing, HTTP 403), a namespace this Prism
Central pins at an older version (networking at v4.0, so NIC profiles and network functions say
which version they need; `:try` asks anyway and meets the server's 404), and namespaces it does
not serve at all (files, opsmgmt, security).

**The estate is invented, and it is fuller than a real v4 API.** The mock answers every kind
the catalog lists, including ones a real Prism Central's v4 collections leave empty because the
console reads them through the older v3 `groups` query (the note under Proof in the README).
On the recorded pc.7.6 these came back with no rows: recovery plans and their jobs, recovery
points, protected resources, entity sync policies, registered domains, images and OVAs. In the
demo they have rows so the Disaster Recovery page and those tables can be seen at all; on your
own Prism Central expect `no recovery plans` where the demo shows two. What the demo proves is
the program, not the API.

What is never touched: your config file, your secret store, your cache, your log. The demo
writes one context file of its own into a private temporary directory (`nutsh-demo-<pid>`
under the system temp directory, mode 0700, removed on exit) and keeps its password - `secret`,
for every account - in memory. It runs with `--no-cache`, so no directory appears under
`$XDG_STATE_HOME/nutsh/cache` and no palette history is kept. The Contexts screen lists two
rows from that file, `demo` and `demo-edge`; the second is the same Prism Central under a
cluster pin (`harbor-edge`), and `a`, `l`, `d`, `:hide`, `:skin`, `:mouse`, `:header` and
`:log`'s level edit that file and no other. Your own `[[guardrails]]` never apply in the demo;
the demo's file carries two rules of its own on `demo-edge` - deleting a VM is refused with a
reason, powering one off asks for its name - so both kinds of rule can be seen, while `demo`
keeps the built-in confirmations.

What does touch the filesystem, as in any session: `:export` writes the file you name in the
working directory, and a log level (`NUTSH_LOG`, or `:log`) writes
`$XDG_STATE_HOME/nutsh/logs/nutsh.log`.

Some keys change what is on the screen and some only produce a task. A power action, a
maintenance action or a migration lands a task in the Tasks table and the row follows it -
POWER flips, a host's state changes - because the mock walks its tasks to completion; a task
can be cancelled while it is running. Nothing else about the estate is a hypervisor: cloning
adds a row, deleting removes one, and that is the whole effect.

The per-namespace explorer opens every kind the catalog has, and the estate does not fill all
of them: a kind with no rows opens an empty table, which is what an empty kind looks like on a
real Prism Central too. Four namespaces are absent altogether (`aiops`, `storage`, `tenancy`,
`objects`) and grey out in the sidebar with the reason, the way an unserved namespace does.

A 401 never occurs - the stored password is right - and neither does a 429: the mock advertises
the client's own ceiling. `ctrl-c` or `:q` quits; when a task you are watching is still running,
it asks first.

## The log

Nothing is written down unless you ask for it. The default is `off`, and a run nobody asked
anything of leaves no file behind.

```sh
NUTSH_LOG=debug nutsh --check      # one run, on stderr
nutsh                             # then `:log` inside the app, or `log = "debug"` in the file
nutsh --info                      # the level, where it came from, and the path
```

Three ways to set it, and the usual precedence: `NUTSH_LOG` beats the config file's `log`, which
beats off. `NUTSH_LOG` takes one of `off`, `error`, `warn`, `info`, `debug`, `trace`, and also
takes a `tracing_subscriber` filter string, so `NUTSH_LOG=nutsh_prism=debug` asks for the
requests and nothing else. Inside the app, `:log` turns it on at `debug` or off again and `:log
trace` names a level outright; either writes the answer to the config file, and the settings
screen shows the level beside where it came from.

Where the lines go depends on the run, not on what you asked for. `--check`, `--info`, `ctx` and
`cache` write to stderr. The TUI cannot: it owns the alternate screen, and a line on stderr
there is a hole in the frame rather than a message, so it writes to
`$XDG_STATE_HOME/nutsh/logs/nutsh.log` instead. `--snapshot` renders a frame to stdout and is
the same case, and so is `nutsh demo`. The file is capped at 4 MB and rolls over one older copy
beside it, so a session left running overnight costs 8 MB and keeps the recent end.

At `debug` every request is one line: the method, the URL, the status, how long it took, and
which credential it went out with - `presented` for the password, `session` for a cookie that
password opened, `refused` for a request this program stopped before the network because the
credential had already been rejected. A 401 or a 5xx is a `warn`, so you do not have to raise
the level to find one.

That last field is what answers "which call was it that failed the creds", and it answers a
second question with it. A 401 on a request that rode a session is that session having ended; a
401 on the request that then presented the password is the password being refused. Both lines
are there, in order:

```
 WARN nutsh_prism::client: request method=GET
   url=https://pc.lab.example:9440/api/volumes/v4.3/config/nvmf-clients status=401 ms=2
   credential=session
 WARN nutsh_prism::client: request method=GET
   url=https://pc.lab.example:9440/api/volumes/v4.3/config/nvmf-clients status=401 ms=1
   credential=presented
```

The second line is the one that matters: that request carried the password and the Prism Central
refused it, which is terminal. Nothing presents it again.

**A log never carries a credential.** Not the `Authorization` header, not the password, not a
`Set-Cookie` or `Cookie` value, not a request body. A Prism Central session cookie is as good as
the password for about fifteen minutes, so it is not written even truncated. A URL is fine; a
header dump is not - and no level and no filter string can turn on a dependency that would
produce one: `hyper` and `h2` log frames, an HPACK frame is a header dump, and only this
program's own crates can reach the writer. `tests/log.rs` drives a session at the highest
verbosity there is and asserts that the exact values that went over the wire appear nowhere in
what came out.

## Requests

Every response carries the rate limit the Prism Central is enforcing, and the token bucket is
driven from those headers rather than from a constant. A credential the Prism Central refuses is
never presented a second time; the pollers stop, the rows on screen stay, and the status line
says so. Five idle minutes stop every poller nobody is reading, and a keystroke starts them
again.

Lists ask for the properties the table actually reads, which takes 68% off a virtual machine
list and 84% off alerts. The status line's meter shows requests a second and how much of the
last minute came from the cache; it turns amber when requests are being paced and red when they
are being refused.

## What the v4 API does not show

nutsh speaks only the v4 REST API. Prism Central's own console does not. For several entity
types the console reads `/api/nutanix/v3/groups`, a generic entity query from the previous API
generation, and those entities have no populated v4 collection behind them. The v4 endpoint
answers truthfully: two hundred, and nothing in it.

So a pane can read `no recovery plans` while the console lists several. That is not a failure to
reach the Prism Central, and it is not a bug in the pane. **An empty v4 collection means the v4
API has none. It does not prove the Prism Central has none.**

Measured against a pc.7.6 Prism Central, both answering `200` with zero entities while the
console showed them:

| screen | what nutsh asks |
|---|---|
| Recovery Plans | `GET /datapolicies/v4.3/config/recovery-plans` |
| Recent DR jobs | `GET /dataprotection/v4.4/config/recovery-plan-jobs` |

The entity types known to live behind v3:

    apps                availability_zones    blueprints    groups
    marketplace_items   projects              protection_rules   recovery_plans

A different cause with the same symptom is a kind whose v4 version this Prism Central does not
serve. Registered Prism Centrals needs `multidomain` v4.3, and a Prism Central pinned at v4.2
has no equivalent path at all, so nutsh greys the entry and names the version it would need
rather than asking a question that cannot be answered.

Buckets are scoped the same way, one level further down:

    GET /objects/v4.1/config/object-stores/{extId}/buckets

That is the local namespace. A federated one is the same call with a header:

    curl -sk -u admin \
      -H 'x-ntnx-objects-namespace: AZ-east-1' \
      https://PC:9440/api/objects/v4.1/config/object-stores/EXTID/buckets

nutsh does not send it, so federated buckets are omitted. The header is not the problem, the
name is: v4 has no endpoint listing federated namespaces, and no object store field names the
one it belongs to. There is nothing to fill it from. Left out until v4 can answer.

This is the call the console makes, for reference. **nutsh does not implement it**:

    curl -sk -u admin https://PC:9440/api/nutanix/v3/groups \
      -H 'content-type: application/json' \
      -d '{"entity_type":"recovery_plan",
           "group_member_attributes":[{"attribute":"name"}]}'

It is left unimplemented on purpose. v3 is a second API surface with its own paging and its own
entity shapes, and this repository holds no published document to generate it from, so every
field would be hand written and unverifiable against a spec. The catalog, the curated columns
and the composed detail are all built on v4 schemas. Adding v3 is a design decision, not a patch.
