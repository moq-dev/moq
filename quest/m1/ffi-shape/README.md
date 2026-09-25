# The bindings mirror Rust's layers

## Goal

A binding consumer finds each layer where Rust keeps it: moq-net at the root,
and `media`, `json`, `audio`, and `video` as their own namespaces, each type
constructed from the lower-layer handle it wraps. `BroadcastProducer` and
`BroadcastConsumer` stop carrying every layer's verbs, and every moq-ffi type
and verb maps to a Rust one. A docs page shows the layers in each language.

## Plan

Lands after the release, as one binding break in Python, Go, Swift, and
Kotlin (Dart is unpublished), so its PRs retarget to `dev`.

Settled shape:

- Groups by role, not crate: the root is moq-net (client, server, session,
  origin, broadcast, track, group); `media` merges hang and moq-mux, since a
  binding never sees that split (catalog, import producers, container
  consumers); `json`, `audio`, and `video` own their producers and consumers.
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
- libmoq renames its C symbols to the same groups (`moq_json_*`,
  `moq_media_*`), with `cpp/obs` adapting.

Each child reshapes one group end to end: moq-ffi, all five wrappers, libmoq,
and the `doc/lib` samples, per the cross-package table. This README owns the
work no child does:

- A layers guide under `doc/lib` mapping net, media, json, audio, and video to
  each language's module, linked from every binding page.
- The bindings section of the following release's upgrade page: old call to
  new call per language.
- `just test interop --all` green on the finished line.

## Quests

- [JSON](/quest/m1/ffi-shape/json.md) - the pilot: json becomes its own namespace wrapping a track in every binding and sets the per-language pattern
- [Net](/quest/m1/ffi-shape/net.md) - client and server take config records, snapshots are records, and the verbs match moq-net
- [Media](/quest/m1/ffi-shape/media.md) - catalog, import, and container consume move under `media`
- [Codecs](/quest/m1/ffi-shape/codec.md) - audio and video encoders and decoders move under their own namespaces with one constructor shape

## Required

- [Release](/quest/m0/release.md) - the restructure follows the release rather than riding it

## Related

- [Track demand](/quest/m1/track-demand.md) - the same `demand()` cleanup in Rust and JS
