# Notes on the Nutanix v4 API

What the v4 API does that its OpenAPI documents do not say, or say somewhere you would not
think to look. Everything here was established against a real Prism Central and is the reason
some piece of this code is shaped the way it is. The engineering invariants that follow from it,
each with the test that holds it up, are in `CONTRIBUTING.md`.

The vendor's own documents are vendored under `specs/`. Where this file and a spec disagree
about a field name, the spec is right and this file is a bug.

## Requests

**Every mutation needs an `NTNX-Request-Id`.** POST, PUT, PATCH and DELETE are all refused
without one. A fresh UUID per request, generated in the client rather than by any caller.

**The ETag is an HTTP header, not a body field.** `If-Match` takes the value of the response's
`ETag` header. `$metadata.ETag` in the JSON body is not it, and reading the body instead
produces a 412 that looks like a concurrent modification.

**A VM power action takes an `If-Match` and no body at all.** `power-on`, `power-off`,
`shutdown` and `reboot` want the header; an empty object as the body is an HTTP 400, "no body
expected". `json=None`, not `json={}`.

**VM delete needs an `If-Match`** - unlike most resources, which delete without one. The ETag
has to be fetched first.

**A sub-resource mutation needs the parent's ETag.** Creating a disk or a NIC on a VM takes the
*VM's* ETag in `If-Match`, not the sub-resource's.

## Bodies

**PUT is full-replace.** Fetch the entity, strip the read-only keys, change what you meant to
change, and send the whole object back. Anything omitted is deleted. The read-only keys are
`$objectType`, `$reserved`, `$metadata`, `extId`, `links`, `tenantId`, `createdBy`,
`creationTime` and `lastModifiedTime`.

**A category PUT is the exception that keeps `ownerUuid`.** Omitting it is an HTTP 400,
"deleting ownerUuid not allowed", so `ownerUuid` is deliberately absent from the read-only list.

**A recovery point needs a non-null `expirationTime`**, as an ISO 8601 string. `null` is an
HTTP 400.

**A reference is an object, not a string.** `ClusterReference` and its siblings are
`type: object, required: [extId], additionalProperties: false`. A bare ext id where the schema
wants a reference is a wrong body, and on a PUT it goes out over the object the Prism Central
already had.

**An address group's `name` is immutable.** Only `description` and the data fields
(`ipv4Addresses` and similar) may be changed by PUT.

**Microseg descriptions are charset-restricted** to `[a-zA-Z0-9 _:.()-]`. No dashes other than
the hyphen, no typographic quotes, no other Unicode.

## Shapes

v4 nests where v3 was flat, and a flat guess reads as a zero or an empty cell rather than as an
error. The paths that are most often guessed wrong:

| what | where it actually is |
|---|---|
| disk size | `disk.backingInfo.diskSizeBytes` |
| disk bus | `disk.diskAddress.busType` |
| volume-group disk | `disk.backingInfo.$objectType` contains `ADSFVolumeGroupReference` |
| NIC MAC | `nic.backingInfo.macAddress`, or `nic.nicBackingInfo.macAddress` |
| NIC IP, assigned | `nic.networkInfo.ipv4Config.ipAddress.value` |
| NIC IP, learned | `nic.networkInfo.ipv4Info.learnedIpAddresses[].value` |
| NIC subnet | `nic.networkInfo.subnet.extId` |
| cluster or host reference | `cluster.extId` - there is no `.name` beside it |
| VM memory | `memorySizeBytes` |
| host CVM address | `controllerVm.externalAddress.ipv4.value` |
| host address | `hypervisor.externalAddress.ipv4.value` |
| template name | `templateName` |

A reference carries an ext id and nothing else, which is why names have to be resolved from the
kind they point at rather than read off the row.

Verify a field path against `specs/{namespace}/{version}.yaml` before writing a renderer for it.
A path that does not exist draws a column of `-`, which no test catches and no user reports.

## Versions

**A namespace is not always one version.** `multidomain` answers
`/v4.3/management/local-domain` while 404ing `/v4.3/config/*`, and serves `config/*` at v4.2.
So a namespace pins at the newest version that answers, and a kind newer than that pin is still
sent at its own version - the server decides, not the client.

**An empty v4 collection means the v4 API has none.** The Prism Central console reads some
entity types through the older v3 `groups` query, and their v4 collections come back empty even
where the console shows rows. This is the API, not the client.

## Rate limits

A Prism Central states its limits on **every** response - a 200, a 404 and a 429 alike - in six
headers that appear in none of the vendored specs:

```text
x-api-ratelimit-limit: 3        x-api-ratelimit-refresh-period-seconds: 1
x-api-ratelimit-remaining: 2
x-ratelimit-limit: 80           x-ratelimit-remaining: 79     x-ratelimit-reset: 0
```

The two triples are different claims and must not be merged. `x-api-ratelimit-*` is a **tier**:
a count and the period it refreshes over, which is a rate. `x-ratelimit-*` is a **budget**: how
much is left, over a window whose length no header gives. There is no honest way to turn "79 of
80 left" into a rate, so a budget may only ever brake. A `reset` of `0` on a barely-touched
budget says nothing and must not be read as "wait for no time".

Three a second is the tightest tier observed. A client running above the advertised tier
provokes intermittent 503s that read as load shedding, 401s arriving minutes after the same
credential succeeded, and - on an account with a lockout policy - lockouts.

The tiers are also declared per operation in the specs, as `x-rate-limit`. They belong to an
operation and not to a path: `/vmm/v4.3/ahv/config/vms/{extId}` is a read at two a second and a
PUT and a DELETE at five.

## Collections

A Prism Central never trims some collections. Tasks, alerts, events and audits accumulate for
the life of the installation - an audit collection of 181 825 rows is 1819 pages at 100 rows a
page, and around 0.85 s a page. Walking one of those to its end is not something a polling loop
can do, so a kind that grows without bound needs a row budget and an order that keeps the rows
worth keeping.

A budget and an action interact. If a kind has both, ask which rows the action is *for* and
whether the budget's ordering excludes them: a newest-first budget over tasks hides a task that
has been running for a month, which is exactly the one somebody wants to cancel.

## Other endpoint quirks

**VM stats need a time range.** The VMM stats endpoint requires `$startTime`, `$endTime`,
`$select` and `$samplingInterval`. Without them it is an HTTP 400.

**Host NICs are listed per cluster.** The host must belong to the cluster named in the path, so
the host-cluster pairing has to be resolved before the list.

**A task names the entity it created, and also the one it acted on.** `entitiesAffected` holds
both, so the newly created entity is the one that is *not* the source.

## TLS

Prism Central's default certificate is self-signed **and carries `basicConstraints CA:TRUE`**.
rustls rejects any end-entity certificate with the CA flag set, whatever the trust store, so
passing that same certificate with `--ca-bundle` fails identically. The options are a CA-signed
certificate on the Prism Central, or `--insecure`. A certificate error has to be reported by its
reason, because the advice differs per reason and `--ca-bundle` is wrong advice for this one.
