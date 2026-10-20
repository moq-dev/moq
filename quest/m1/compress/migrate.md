# [M] One compressed track per name

## Goal

Every first-party compressed track is one name with `compress: true`:
`catalog.json` in moq-mux and js/publish, each moq-stats `.json` track, and
the hang timeline. `catalog.json.z`, the stats `.z` siblings,
`<track>.timeline.z`, hang's catalog `compression` field, the "hangz"
consumer options, the moq-flate crate, `@moq/flate`, `moq-mux::binary`,
moq-json's and `@moq/json`'s `Config::compression`, and the bindings' `flate`
namespace are deleted.

## Plan

See the [epic](/quest/m1/compress/README.md) for the decisions.

- Delete the `.z` names outright, with no transition release: hangz is opt-in
  and `main` takes breaking changes. Add the MIGRATE.md entries.
- Publishers: `rs/moq-mux/src/catalog/producer.rs`, `js/publish`
  (`broadcast.ts`, `catalog.ts`), `rs/moq-stats` (drop `TrackPair`), and
  `rs/hang/src/timeline.rs` set `compress` on their one track.
- Consumers: `rs/moq-mux/src/catalog/consumer.rs`, `rs/moq-cli`
  (`subscribe.rs`, `archive.rs`), and `js/watch` lose their hangz paths.
- moq-stats deltas: the `.z` side uses merge-patch deltas and the plain side
  does not. Keep deltas on the single track unless measuring stats bytes with
  and without them on a compressed track shows the saving is small; then
  drop them, since all they save is some decode CPU. Report the numbers in
  the PR.
- moq-json and `@moq/json` encoders and decoders drop `Config::compression`
  and their direct codec calls (`snapshot`, `window`, `stream`). JSON tracks
  compress by default (maintainer, 2026-10-17: "always useful"): the default
  lives where a track is created: moq-mux's JSON helpers and the docs' JSON
  examples set `compress: true`. `Producer::new(track, config)` keeps
  wrapping a caller-built track, per `rs/moq-ffi/AGENTS.md`'s layers,
  and its docs say to set `compress` on that `Info`. Delete `rs/moq-mux/src/binary.rs` and its
  users; opaque bytes are a plain track.
- Delete `rs/moq-flate`, `js/flate`, `rs/hang/src/catalog/compression.rs`,
  and the moq-ffi flate tracks with every wrapper's `flate` namespace and
  `doc/lib/*` entry, per the cross-package sync table.

- Hang format: drop the `compression` field and the `catalog.json.z` and
  `.z` sibling text from `drafts/draft-lcurley-moq-hang.md` (validate with
  `just drafts check`), the `js/hang` catalog schema, and `doc/concept`, in
  step with `rs/hang`.

Public API: removals listed above. Wire: the hang catalog loses
`compression` and the `.z` tracks, in its draft too; moq-net none.

## Required

- [Core](/quest/m1/compress/core.md) - Rust compresses opted-in tracks
- [JavaScript](/quest/m1/compress/js.md) - js/publish and js/watch need the JS side before they drop `.z`
