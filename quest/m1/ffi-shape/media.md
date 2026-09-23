# [L] Catalog, import, and container consume move under media

## Goal

`media` owns what hang and moq-mux do for a binding: the catalog, the import
producers, and the container consumers, each constructed from a broadcast.
`BroadcastProducer` loses `publish_{audio,video,container}` and their
`_on_track`/`_stream` variants plus the catalog setters;
`BroadcastConsumer` loses `subscribe_catalog`, `subscribe_media`, and
`fetch_media_group`.

## Plan

Mirror moq-mux's import names and hang's catalog. The catalog setters
(`set_video_properties`, `set_catalog_section`, `remove_catalog_section`)
belong on a catalog type, not the broadcast. The `_on_track` variants take a
`TrackRequest`; decide whether that is a second constructor or an argument
once the shape is in front of you, and prefer one path. Go's
`FetchMediaGroup` takes an options struct. Media producers watch subscribers
through `demand()` only.

libmoq's media symbols move to `moq_media_*`, and `cpp/obs` adapts.

Public API: breaking in every binding and libmoq. Wire: none.

## Required

- [JSON](/quest/m1/ffi-shape/json.md) - sets the per-language namespace pattern
