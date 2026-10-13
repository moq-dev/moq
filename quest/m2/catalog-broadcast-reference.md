# [M] MSF conversion and the GStreamer source honor a rendition's broadcast

## Goal

A hang rendition can live in another broadcast (`broadcast`, relative to the
catalog's own). Three readers ignore that today and resolve the track in the
catalog's own broadcast instead, so they silently play or advertise the wrong
track:

- MSF conversion (`to_msf_media` in `rs/moq-mux/src/catalog/producer.rs`)
  emits a cross-broadcast rendition as a track in the catalog's own
  namespace, and `from_msf` ignores an MSF track's `namespace`.
- The GStreamer source (`rs/moq-gst/src/source/imp.rs`) subscribes every
  rendition on the catalog's broadcast.
- The browser MSF reader: `js/msf/src/catalog.ts` drops `namespace`, so
  `toHang` in `js/watch/src/msf.ts` builds renditions without `broadcast`,
  and `@moq/watch` subscribes them on the catalog's broadcast whenever the
  catalog format is MSF.

Each either honors the reference or refuses the rendition loudly. None
guesses.

## Plan

- MSF: model the track `namespace` in `moq-msf` (it is parsed and dropped
  today) and map it to and from the relative `broadcast`, using the
  catalog broadcast's path as the base the way the hang consumer does. If a
  reference cannot be expressed (it walks above the root, or the producer
  has no path), leave that rendition out with an error rather than publish
  it under the wrong namespace.
- GStreamer: subscribe through the same relative-path resolution the moq-mux
  container sources use (`subscribe_track(config.broadcast, track)`), so a
  pad's identity includes the broadcast as well as the track.
- JS: model `namespace` in `js/msf` and map it to `broadcast` in `toHang`,
  mirroring the Rust mapping.
- Tests: a rendition with `broadcast: "./left"` round-trips through MSF with
  its namespace in Rust and JS, the GStreamer source subscribes on the
  referenced broadcast, and `@moq/watch` subscribes a cross-broadcast MSF
  rendition on its own broadcast.

All three gaps predate #5367, which added the per-rendition `track` name and the
shared `hang::catalog::Reference` resolver these readers should use. Start
after it merges.

Public API: `moq_msf::Track` in Rust and `@moq/msf`'s `TrackSchema` and
`Track` in JS gain `namespace`. Wire: MSF catalogs carry
`namespace` for cross-broadcast renditions; hang is unchanged.

## Related

- [Catalog references pin their broadcast instances](/quest/m0/broadcast-epoch/catalog-references.md) - the same references gain epochs; whichever lands second carries them through MSF and GStreamer
- [Sibling broadcasts on one clock](/quest/m2/sibling-broadcasts.md) - the sibling-broadcast pattern, which does not use catalog references
