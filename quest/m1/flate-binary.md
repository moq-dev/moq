# [M] moq-binary folds into moq-flate

## Goal

Opaque binary tracks live in `moq-flate` and `@moq/flate`: the `snapshot` and
`stream` modes `moq-binary` and `@moq/binary` provide today move there beside
the group-scoped codec, and `moq-binary` and `@moq/binary` are deleted. One
package owns compressed and opaque tracks, so there is no second "compressed
track" wrapper to build.

## Plan

Decided in the 2026-09-28 quest audit: `moq-binary` already composes
`moq-flate` into per-group windows (each group one sync-flushed DEFLATE
stream), which is what the m2 flate line planned to add as a new track wrapper.
Folding the two removes the duplicate instead of building it.

- Rust: move `rs/moq-binary/src/{snapshot,stream}` and `Compression` into
  `moq-flate` as `moq_flate::{snapshot, stream}`, keeping the codec
  (`Encoder`/`Decoder`) at the root. `moq-flate` gains the `moq-net`
  dependency. Delete `rs/moq-binary` and its workspace member, and repoint
  `moq-mux` (`src/binary.rs`, `src/error.rs`) and `rs/libmoq` if it still
  exists.
- JS: move `js/binary/src/{snapshot,stream,compression.ts}` into `@moq/flate`
  as `Snapshot` and `Stream`, adding the `@moq/net` and `@moq/signals`
  dependencies, and delete `js/binary`.
- Wire and catalog: unchanged. The hang catalog's `binary` section and
  `moq_mux::binary` keep their names; they describe the track's content, and
  the catalog section is wire.
- moq-ffi: rename `binary.rs` and its `publish_binary_*`, `MoqBinaryConfig`,
  and producer types after `flate`, so every binding names the crate it wraps.
  If [FFI shape](/quest/m1/ffi-shape/README.md) has already given them a
  `flate` namespace, follow it instead.
- Open: whether `Compression::None` survives the move. An uncompressed opaque
  track still needs a home, so the recommendation is to keep it and document
  that the crate name is not a promise every track is deflated.
- Docs: fold `doc/lib/rs/moq-binary.md` into a `moq-flate` page and
  `doc/lib/js/binary.md` into a `@moq/flate` page, fix `doc/.vitepress/config.ts`,
  `doc/lib/{rs,js}/index.md`, `doc/concept/hang.md`, the android workflow path
  filter, and add an upgrade note in `doc/setup/upgrade.md`. Grep for
  `moq-binary`, `moq_binary`, and `@moq/binary`.

Public API: breaking. `moq-binary` and `@moq/binary` are published and
deleted, and moq-ffi's binary names change, so this lands on `dev`.
`moq-flate` and `@moq/flate` grow additively. Wire: none.

## Related

- [Compressed tracks](/quest/m2/flate/README.md) - the hand-written wrappers expose these tracks
- [FFI shape](/quest/m1/ffi-shape/README.md) - gives the flate tracks their binding namespace
