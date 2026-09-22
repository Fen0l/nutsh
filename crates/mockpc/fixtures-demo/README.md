# fixtures-demo

The estate `nutsh demo` boots: one directory per site, one JSON file per list path, every
file embedded in the `nutsh-mockpc` library by `src/demo.rs` (`DEMO`, later `DEMO_DR`). A file
on disk that the table does not name fails `tests/demo.rs::embedded_tables_match_the_trees_on_disk`;
a table entry with no file fails to compile.

Invented, produced by hand, shapes copied from `fixtures-lab/` (a `pc.7.6` recording). Nothing
here was recorded from any Prism Central.

## What the scanners refuse

1. `xtask/tests/fixtures_are_clean.rs` walks every string value of every `.json` here
   (`xtask/src/record.rs`, `leaks`). An IPv4 address anywhere in a string must be in
   `192.0.2.0/24`, `198.51.100.0/24` or `203.0.113.0/24`; a MAC must start `00:00:5e:00:53:`;
   an IPv6 address must start `2001:db8:`; an e-mail must end `@example.invalid`
   (`example.com` mail is refused). Keys ending in `version` and keys starting with `$` are not
   scanned, so `10.3.1.2` is fine under `entityVersion` and nowhere else.
2. `xtask/tests/tree_is_clean.rs` reads every tracked file as text and hashes each token
   (each part between dots and hyphens, and its alphabetic stem) against a list of hashed
   names. A hit prints the file, the line and the token: rename it. It also refuses any host
   name ending `.local`, `.lan`, `.localdomain` or `.intranet`.
3. Host names are `*.example.com`; the demo's DHCP domain is `demo.example.com`.
4. Nothing here may name real infrastructure, a real person or a real organisation, in a
   name, a message or a description.

## Rules every file follows

- One instant: every timestamp is written around `T0 = 2026-09-05T10:00:00Z` and, except
  `expirationTime`, is strictly before `2026-09-05T09:59:00Z`. `src/demo.rs` shifts every
  timestamp by `now - T0` at load.
- Timestamps are canonical RFC 3339: `YYYY-MM-DDTHH:MM:SSZ`, whole seconds, `Z`. Any other
  spelling is not shifted at load and `tests/demo.rs` refuses it.
- Microsecond integers (`bootTimeUsecs`) are Unix microseconds around `T0` and are shifted too.
- ExtIds are `XX000000-<site>-4000-8000-0000000000NN`: `XX` is the kind tag from the table
  in `tasks/2026-09-21-nutsh-demo-plan-2a-harbor-estate.md` (not tracked), `<site>` is `0a01`
  for `demo` and `0b02` for `demo-dr`, `NN` the row's ordinal. Tasks are
  `ZXJnb24=:7a5c0000-<site>-4000-8000-0000000000NN`. Storage containers are keyed by
  `containerExtId`, buckets by `name`.
- Every string under a key `extId`, `uuid`, `*ExtId`, `*Uuid`, `*UUID`, and every string inside
  `*ExtIds[]` / `*Uuids[]`, is either the row's own id, the id of a row in the same site, the
  nil UUID `00000000-0000-0000-0000-000000000000` (the Prism Central's "none"), or, under a
  `*domainManagerExtId` key, the other site's domain-manager id. Keys that could only point at
  something no list holds (`biosUuid`, `generationUuid`, `storagePoolExtId`, `scheduleExtId`,
  `groupUuid`, `availableVersionUuid`, `nodeExtIds` on the domain manager) are left out.
- `$objectType` is spelled the `pc.7.6` way: `<namespace>.v4.<segments>.<Schema>`
  (`vmm.v4.ahv.config.Vm`, `clustermgmt.v4.config.Host`, `prism.v4.config.Task`).
- No key named `password`, anywhere.
- Child lists the catalog reaches by drilling into a VM (`disks`, `nics`, `cd-roms`, `gpus`,
  `serial-ports`), a cluster (`hosts`, `hosts/<h>/host-nics`) or a task (`affected-entities`)
  are **not** files: `src/demo.rs` derives them from the parent row at load.
