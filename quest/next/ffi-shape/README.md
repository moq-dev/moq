# The bindings mirror Rust's layers

## Goal

A binding consumer finds each layer where Rust keeps it: moq-net at the root,
and `media`, `json`, `audio`, and `video` as their own namespaces, each type
constructed from the lower-layer handle it wraps. `BroadcastProducer` and
`BroadcastConsumer` stop carrying every layer's verbs, and every moq-ffi type
and verb maps to a Rust one. A docs page shows the layers in each language.

## Plan

Lands after the release, as one binding break in Python, Go, Swift, and
Kotlin (Dart is unpublished). Starting it moves the line under
[dev](/quest/dev/README.md).

Settled shape:

- Groups by role, not crate: the root is moq-net (client, server, session,
  origin, broadcast, track, group); `media` merges hang and moq-mux, since a
  binding never sees that split (catalog, import producers, container
  consumers); `json`, `audio`, and `video` own their producers and consumers.
- A layer's type is constructed from the handle below it, like Rust
  (`moq_json::snapshot::Producer::new(track)`), not reached through an
  accessor on the broadcast. Sketch, not a contract:
  `json.SnapshotProducer(broadcast, name)`, `video.Encoder(broadcast, config)`.
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
- `just test smoke --all` green on the finished line.

## Quests

- [JSON](/quest/next/ffi-shape/json.md) - the pilot: json becomes its own namespace in every binding and sets the per-language pattern
- [Net](/quest/next/ffi-shape/net.md) - client and server take config records, snapshots are records, and the verbs match moq-net
- [Media](/quest/next/ffi-shape/media.md) - catalog, import, and container consume move under `media`
- [Codecs](/quest/next/ffi-shape/codec.md) - audio and video encoders and decoders move under their own namespaces with one constructor shape

## Required

- [Release](/quest/dev/release.md) - the restructure follows the release rather than riding it

## Related

- [Track demand](/quest/next/track-demand.md) - the same `demand()` cleanup in Rust and JS
- [Binding docs](/quest/next/binding-docs.md) - its nightly sample check guards the rewritten pages
