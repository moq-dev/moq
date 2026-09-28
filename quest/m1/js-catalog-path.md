# [XS] JS catalog watch rejects escaping broadcast references

## Goal

`Catalog.watch` in `@moq/hang` rejects a catalog whose rendition or track
`broadcast` reference escapes the broadcast, as Rust `Catalog::subscribe`
does with `EscapingBroadcast` ([#3935](https://github.com/moq-dev/moq/pull/3935)).
JS cannot check today: `watch` takes a `Broadcast.Consumer`, which has no
path to resolve the reference against, so `../../../other` passes in JS and
fails in Rust.

## Plan

Decided: make the path public on `@moq/net`'s `Broadcast.Consumer` and have
`Catalog.watch` read it, so the `watch()` call shape does not change.

Guidance:

- Mirror Rust's `broadcast::Info::path`: the origin stamps each handle with
  the path it was requested or announced at, relative to the cursor's root,
  and a standalone broadcast has an empty path, so any `..` escapes.
- Resolve every video, audio, text, JSON, and binary reference with
  `Path.tryResolve` against it and reject the update when it returns
  `undefined`, as `js/hang/src/catalog/path.ts` already describes. Keep the
  relative values in the yielded catalog unchanged.
- Tests: an escaping reference is rejected, a sibling reference under the
  same root passes, and a standalone broadcast rejects `..`.
