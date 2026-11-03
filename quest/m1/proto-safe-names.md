# [S] Prototype-safe rendition names in JS

## Goal

JS keeps names that collide with `Object.prototype` (`__proto__`,
`constructor`, `toString`) wherever it keys renditions or catalog members
by name, the way Rust does:

- `js/msf/src/catalog.ts`'s `ext` map keeps a `__proto__` key as an own
  property, like `moq-msf`'s `Catalog::ext`.
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
[moq-dev/moq#7498](https://github.com/moq-dev/moq/pull/7498): a plain object
keyed by an untrusted name. Planned as one quest (maintainer, from #7498's
iterate, 2026-11-03).

- **MSF `ext`.** It parses with `z.record` (around line 76), which drops a
  `__proto__` key. Use the same parser and test corpus as hang's maps from
  #7498.
- **js/publish.** `#renditions` and `#records` in
  `js/publish/src/broadcast.ts` (around lines 147 and 163) are plain
  objects, so `this.#renditions.peek()[name]` finds `Object.prototype`'s
  member and throws "already registered". Key them with a `Map` (or
  null-prototype objects) and carry that through the catalog and stats
  builders that read them, so the published catalog keeps the name.
- Test `__proto__`, `constructor`, and `toString` in each place.

Public API: none. Wire: none.

## Required

- [Text and data sections keep a prototype-named ID](/quest/m1/cut-audit/catalog-proto-ids.md) - the parser and corpus this reuses (moq-dev/moq#7498)
