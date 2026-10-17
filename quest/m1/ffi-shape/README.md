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

- Children PR directly to `main`, so moving a binding layer does not require
  hand-porting concurrent moq-ffi changes onto a separate line branch.
- The bindings still break once per release, not once per merge: binding
  releases (moq-ffi and the Python, Go, Swift, Kotlin, Dart, and C++
  packages) wait until the codec namespace reshape merges
  (confirmed as a hard release gate in the 2026-10-10 audit), joining the
  named error fields and the broadcast-epoch renames (`connects()`,
  `reanchor()`) already on `main`. Codecs ranks first below for that reason.
  Dart is published (`moq` 0.1.0, `moq_ffi` 0.4.x), so
  its renames get the same upgrade notes as the others.
- The C++ package (#4079) landed first, so #4519 ported `cpp/moq`, `cpp/obs`,
  and the C++ interop client. C++ is one flat `moq::` namespace of the
  generated types, with moq-ffi's `Moq` prefix stripped (`moq::MediaTrackConsumer`):
  uniffi-bindgen-cpp renders one namespace per crate, so a `media::` namespace
  would need the generator to map a name prefix to a namespace.

Settled shape:

- Groups by role, not crate: the root is moq-net (client, server, session,
  origin, broadcast, track, group); `media` merges hang and moq-mux, since a
  binding never sees that split (catalog, import producers, container
  consumers); `json`, `flate`, `audio`, and `video` own their producers and
  consumers. `flate` holds the opaque snapshot and stream tracks, named after
  the `moq-flate` crate.
- A layer reuses the net noun it decodes, and its namespace tells them apart:
  `media.TrackConsumer` and `media.GroupConsumer` read what
  `media.TrackProducer` writes, a level above the raw `TrackConsumer` and
  `GroupConsumer` (decided 2026-10-13 on #5356, replacing the 5-word
  `MoqMediaContainerGroupConsumer`).
- A layer's type is constructed from the handles its Rust constructor takes,
  not reached through an accessor on the broadcast: JSON wraps a track
  (`moq_json::snapshot::Producer::new(track, config)`), so it also works on a
  track accepted from a request; the codecs take the broadcast and its
  catalog. JSON and flate producers take the broadcast too, whose catalog
  advertises the track, as `moq_mux::catalog::Producer::json_snapshot` does. Sketch, not a contract: `json.SnapshotProducer(track, config)`,
  `video.Producer(broadcast, catalog, config)` for Rust's
  `encode::Producer`, and the codec-only `video.Encoder(config)` for
  `encode::Encoder` (decided 2026-10-06, see
  the codec namespace reshape).
- UniFFI 0.32 allows one namespace per crate, so moq-ffi groups by type and
  the wrappers supply real namespaces in each language's idiom: Python
  submodules, Go subpackages (`moq.dev/moq/json`, aliased on import next to
  `encoding/json`), Kotlin packages, Dart libraries, Swift caseless-enum
  namespaces. A per-layer crate split, reopened 2026-10-13 to move the
  namespaces below the wrappers, was spiked and rejected (#5460): UniFFI
  0.32 keys a crate's types by bare name, so `media.GroupConsumer` cannot
  take the net `GroupConsumer` it wraps. Python drops the media type
  silently; every other generator refuses. Distinct Rust names renamed
  back per language work only in Python and Go: Kotlin, Swift, Dart, and
  C++ shadow or misplace the external type, and generated C refuses
  external types. Revisit once
  [namespaced external types](/quest/m2/uniffi-namespaces.md) fixes
  [mozilla/uniffi-rs#3026](https://github.com/mozilla/uniffi-rs/issues/3026)
  in the moq-dev/uniffi fork.
- `demand()` is the one way to watch subscribers; producers drop their
  `name`/`is_used`/`used`/`unused` duplicates.
- moq-ffi and its generated consumers, including `cpp/obs` (above). The
  hand-written moq-c is out of scope: the
  [generated C](/quest/m1/c/README.md) and C++
  bindings inherit this shape from moq-ffi, so reshaping the hand-written C
  ABI would break C users twice. The narrow
  [claim-epoch correctness exception](/quest/m1/claim-answer-epochs.md) does
  not expand this layer reshaping to the hand-written ABI.
- `rs/moq-ffi/AGENTS.md` records the per-language namespace pattern `json`
  set.

Each child reshapes one group end to end: moq-ffi, all five wrappers, the
C++ package and `cpp/obs`, the `doc/lib` samples per the cross-package table,
and its entries in the Unreleased section of `doc/setup/upgrade.md`. Each
runs `just test interop --all`.

## Required

- [Stream importer name](/quest/m1/ffi-shape/stream-producer-name.md) - the maintainer settles `media.ContainerStreamProducer`'s name before the binding release
