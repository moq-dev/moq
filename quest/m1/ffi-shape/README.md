# The bindings mirror Rust's layers

## Goal

A binding consumer finds each layer where Rust keeps it: moq-net at the root,
and `media`, `json`, `flate`, `audio`, and `video` as their own namespaces, each type
constructed from the lower-layer handle it wraps. `BroadcastProducer` and
`BroadcastConsumer` stop carrying every layer's verbs, and every moq-ffi type
and verb maps to a Rust one. A docs page shows the layers in each language.

## Plan

Lands after the release, as one binding break in Python, Go, Swift, Kotlin,
and Dart. Dart is published now (`moq` 0.1.0 and `moq_ffi` 0.4.x on pub.dev),
so its renames get the same upgrade note as the others.

Decided in the 2026-10-05 audit:

- The m0 [Bindings](/quest/m0/broadcast-epoch/bindings.md) quest lands
  first. This line rebases onto it and adopts its epoch surface and the
  `session.epoch()` rename, rather than renaming again.
- This line lands before [C++ through moq-ffi](/quest/m1/cpp/README.md),
  which then ports `cpp/moq`, `cpp/obs`, and the C++ interop client onto the
  reshaped moq-ffi, so the C++ breaks once.

Settled shape:

- Groups by role, not crate: the root is moq-net (client, server, session,
  origin, broadcast, track, group); `media` merges hang and moq-mux, since a
  binding never sees that split (catalog, import producers, container
  consumers); `json`, `flate`, `audio`, and `video` own their producers and
  consumers. `flate` holds the opaque snapshot and stream tracks moq-ffi
  publishes as `publish_flate_*` today, named after the `moq-flate` crate.
- A layer's type is constructed from the handles its Rust constructor takes,
  not reached through an accessor on the broadcast: JSON wraps a track
  (`moq_json::snapshot::Producer::new(track, config)`), so it also works on a
  track accepted from a request; the codecs take the broadcast and its
  catalog. Sketch, not a contract: `json.SnapshotProducer(track, config)`,
  `video.Encoder(broadcast, catalog, config)`.
- UniFFI 0.32 allows one namespace per crate, so moq-ffi groups by type and
  the wrappers supply real namespaces in each language's idiom: Python
  submodules, Go subpackages (`moq.dev/moq/json`, aliased on import next to
  `encoding/json`), Kotlin packages, Dart libraries, Swift caseless-enum
  namespaces.
- `demand()` is the one way to watch subscribers; producers drop their
  `name`/`is_used`/`used`/`unused` duplicates.
- moq-ffi and its generated consumers. The C++ line ports `cpp/obs` after
  this lands (above). The hand-written moq-c is out of scope: the
  [generated C](/quest/m1/c/README.md) and [C++](/quest/m1/cpp/README.md)
  bindings inherit this shape from moq-ffi, so reshaping the hand-written C
  ABI would break C users twice.

Each child reshapes one group end to end: moq-ffi, all five wrappers, and the
`doc/lib` samples, per the cross-package table. This README owns the
work no child does:

- A layers guide under `doc/lib` mapping net, media, json, flate, audio, and video to
  each language's module, linked from every binding page.
- The bindings section of the following release's upgrade page: old call to
  new call per language.
- `just test interop --all` green on the finished line.

## Required

- [JSON](/quest/m1/ffi-shape/json.md) - the pilot: json and flate become their own namespaces wrapping a track in every binding and set the per-language pattern
- [Net](/quest/m1/ffi-shape/net.md) - client and server take config records, snapshots are records, and the verbs match moq-net
- [Media](/quest/m1/ffi-shape/media.md) - catalog, import, and container consume move under `media`
- [Codecs](/quest/m1/ffi-shape/codec.md) - audio and video encoders and decoders move under their own namespaces with one constructor shape
