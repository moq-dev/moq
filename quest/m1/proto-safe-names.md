# [S] Prototype-safe rendition names in JS

## Goal

JS keeps names that collide with `Object.prototype` (`__proto__`,
`constructor`, `toString`) wherever it keys renditions or catalog members
by name, the way Rust does:

- `js/msf/src/catalog.ts`'s exported `CatalogSchema` keeps a `__proto__`
  `ext` key as an own property, as its `decode` already does, like
  `moq-msf`'s `Catalog::ext`.
- `js/publish` registers a rendition named `__proto__` or `constructor`
  instead of refusing it as "rendition already registered", and its catalog
  and stats builders publish it.

The shared parity corpus covers each.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- ✅ m1
- m2

Both gaps are the same bug as hang's catalog maps, fixed by
[moq-dev/moq#7498](https://github.com/moq-dev/moq/pull/7498) (merged as
[7b4f53165](https://github.com/moq-dev/moq/commit/7b4f53165056d665378a09ec84465b6b033f5bf7)):
a plain object keyed by an untrusted name. Planned as one quest (maintainer, from #7498's
iterate, 2026-11-03).

- **MSF `ext`.** `decode` and `encode` already keep a `__proto__` member
  (null-prototype maps; `catalog.test.ts`'s "round-trips a __proto__
  extension member as data"). Only the exported `CatalogSchema` (around line
  69) still declares `ext` as a `z.record`, which drops it for a caller that
  parses with the schema directly. Give it the same null-prototype parse, or
  stop exporting it as a parser if nothing outside `js/msf` should use it.
- **js/publish.** `#renditions` and `#records` in
  `js/publish/src/broadcast.ts` (around lines 147 and 163) are plain
  objects, so `this.#renditions.peek()[name]` finds `Object.prototype`'s
  member and throws "already registered". Key them with a `Map` (or
  null-prototype objects) and carry that through the catalog and stats
  builders that read them, so the published catalog keeps the name.
- Test `__proto__`, `constructor`, and `toString` in each place that changes.

Public API: none. Wire: none.
