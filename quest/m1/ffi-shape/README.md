# The bindings mirror Rust's layers

## Goal

A binding consumer finds each layer where Rust keeps it: moq-net at the root,
and `media`, `json`, `flate`, `audio`, and `video` as their own namespaces, each type
constructed from the lower-layer handle it wraps. `BroadcastProducer` and
`BroadcastConsumer` stop carrying every layer's verbs, and every moq-ffi type
and verb maps to a Rust one. A docs page shows the layers in each language.

## Plan

The line landed on `main` in #4519 (2026-10-09) with its json/flate, net,
media, and request-accept children, and the remaining children PR straight to
`main`. Decided by the maintainer, 2026-10-09:

- Land now rather than keep collecting on the line branch, because questline
  branches are retiring ([Flat questlines](/quest/m1/quest-flat-lines.md)) and
  each `main` merge meant hand-porting moq-ffi changes onto moved code.
- The bindings still break once per release, not once per merge: binding
  releases (moq-ffi and the Python, Go, Swift, Kotlin, Dart, and C++
  packages) wait until [Codecs](/quest/m1/ffi-shape/codec.md),
  [Bindings](/quest/m0/broadcast-epoch/bindings.md)' `epoch()` rename, and
  [named error fields](/quest/m1/ffi-shape/error-fields.md) all merge
  (confirmed as hard release gates in the 2026-10-10 audit). Codecs ranks
  first below for that reason. Dart is published (`moq` 0.1.0, `moq_ffi` 0.4.x), so
  its renames get the same upgrade notes as the others.
- Bindings no longer lands first (reversing the 2026-10-05 audit): it adopts
  the reshaped wrappers on `main` instead of this line rebasing onto it.
- The C++ package (#4079) landed first, so #4519 ported `cpp/moq` (including
  the hand-kept `moq::` aliases `just cpp check` audits), `cpp/obs`, and the
  C++ interop client. C++ stays a flat `moq::` namespace with `Media*` type
  names until [unprefixed names](/quest/m1/cpp-generated-shape.md) lands.

Settled shape:

- Groups by role, not crate: the root is moq-net (client, server, session,
  origin, broadcast, track, group); `media` merges hang and moq-mux, since a
  binding never sees that split (catalog, import producers, container
  consumers); `json`, `flate`, `audio`, and `video` own their producers and
  consumers. `flate` holds the opaque snapshot and stream tracks, named after
  the `moq-flate` crate.
- A layer's type is constructed from the handles its Rust constructor takes,
  not reached through an accessor on the broadcast: JSON wraps a track
  (`moq_json::snapshot::Producer::new(track, config)`), so it also works on a
  track accepted from a request; the codecs take the broadcast and its
  catalog. JSON and flate producers take the broadcast too, whose catalog
  advertises the track, as `moq_mux::catalog::Producer::json_snapshot` does. Sketch, not a contract: `json.SnapshotProducer(track, config)`,
  `video.Producer(broadcast, catalog, config)` for Rust's
  `encode::Producer`, and the codec-only `video.Encoder(config)` for
  `encode::Encoder` (decided 2026-10-06, see
  [Codecs](/quest/m1/ffi-shape/codec.md)).
- UniFFI 0.32 allows one namespace per crate, so moq-ffi groups by type and
  the wrappers supply real namespaces in each language's idiom: Python
  submodules, Go subpackages (`moq.dev/moq/json`, aliased on import next to
  `encoding/json`), Kotlin packages, Dart libraries, Swift caseless-enum
  namespaces.
- `demand()` is the one way to watch subscribers; producers drop their
  `name`/`is_used`/`used`/`unused` duplicates.
- moq-ffi and its generated consumers, including `cpp/obs` (above). The
  hand-written moq-c is out of scope: the
  [generated C](/quest/m1/c/README.md) and C++
  bindings inherit this shape from moq-ffi, so reshaping the hand-written C
  ABI would break C users twice.
- `rs/moq-ffi/AGENTS.md` records the per-language namespace pattern `json`
  set.

Each child reshapes one group end to end: moq-ffi, all five wrappers, the
C++ package and `cpp/obs`, the `doc/lib` samples per the cross-package table,
and its entries in the Unreleased section of `doc/setup/upgrade.md`. Each
runs `just test interop --all`.

## Required

- [Codecs](/quest/m1/ffi-shape/codec.md) - audio and video producers, codec-only encoders, and decoders move under their own namespaces, named as in Rust; binding releases wait for it
- [Named error fields](/quest/m1/ffi-shape/error-fields.md) - `MoqError` variants name their fields, so no binding exposes a positional `v1`
- [Bindings](/quest/m0/broadcast-epoch/bindings.md) - the wrappers expose epochs and rename `session.epoch()`, on the reshaped wrappers
- [Group request demand](/quest/m1/ffi-shape/group-request-demand.md) - `MoqGroupRequest::demand()` in moq-ffi and every wrapper
- [Track request demand](/quest/m1/ffi-shape/track-request-demand.md) - `MoqTrackRequest::demand()` in moq-ffi and every wrapper, built on group request demand and coordinated with Bindings
- [Layers guide](/quest/m1/ffi-shape/layers-guide.md) - a `doc/lib` page maps each Rust layer to every binding's module, once Codecs adds `audio` and `video`
