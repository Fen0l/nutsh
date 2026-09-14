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

[refresh]
default = 30             # seconds, or "auto" for each kind's own rhythm, or "off"

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
the same case. The file is capped at 4 MB and rolls over one older copy beside it, so a session
left running overnight costs 8 MB and keeps the recent end.

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
