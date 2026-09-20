# [M] Reading a catalog is one call in both languages

## Goal

A consumer subscribes to a broadcast's catalog with one call and iterates
typed roots, in Rust and JS. Today Rust is
`source.track(Catalog::DEFAULT_NAME)?.subscribe(Catalog::default_subscription()).await?`
then `moq_mux::catalog::hang::Consumer::<E>::new(track)`, and `@moq/hang`
exports only schemas, so moq.pro's app subscribes to `"catalog.json"` by
name and runs its own read loop.

## Plan

- `hang::Catalog::subscribe(&broadcast::Consumer) -> catalog::Consumer<E>`
  in Rust; `Catalog.watch(broadcast): AsyncIterable<Root>` in `@moq/hang`.
- JS `Hang.Timeline` gains a `Consumer` yielding `{ push } | { pop } | { skip }` to mirror the
  Rust `Event`; nothing in JS reads `archive`, `clock`, or `wallClockTime`
  today.
- `hang::container::MAX_AGE` is public; moq.pro's fleet config cites it by
  a name that no longer exists.
- `@moq/json` consumers stop swallowing `readFrame` errors and expose the
  same typed gaps the window consumer has (`Desync`, `MissingSnapshot`);
  `Stream.Config` takes a `schema` like snapshot. moq.pro wraps `next()` in
  a failure counter because a dead track and a bad frame reject alike.

Public API: additive on hang, @moq/hang, @moq/json. Wire: none.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - starts on main

## Related

- [JSON merge](/quest/m2/json-merge.md) - the snapshot patch consumer
