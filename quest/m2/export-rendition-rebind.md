# [M] Exporters follow a rendition to a new track

## Goal

When a catalog update keeps a rendition's ID but changes its resolved
`track` or `broadcast`, the fMP4, TS, MKV and FLV exporters rebuild that
rendition from the new track, as a remove plus an add. Today each exporter
resolves the track once, when it builds the rendition's `ExportSource`, and
keys it by ID afterwards, so it keeps muxing the old track (or stops when
that track ends) while the catalog names the new one.

## Plan

- Keep each source's resolved identity (broadcast reference and track name)
  beside it, and compare it on every catalog update. A change drops the old
  source and builds a new one, through the same path a newly added
  rendition takes in that muxer: fMP4 `Fmp4Track::reconfigure`, the TS
  `old.remove(name)` reuse, and the MKV and FLV bound-ID checks.
- A track change is a new track, so nothing carries over from the old one:
  no resumed sequence numbers, timestamps or continuity counters. Each
  muxer signals the switch the way it signals a removed and re-added
  rendition.
- Tests per exporter: a rendition `main` moved from `video-a` to
  `video-b` exports `video-b`'s frames after the update and none of
  `video-a`'s, and a `broadcast`-only change does the same.

Raised by a Codex review of #5367, which added the per-rendition `track`
field; the `broadcast` gap predates it. Deferred from that PR by the
maintainer's recommended option. Starts after #5367 merges.

Public API: none. Wire: none.

## Related

- [A failed rendition fails alone](/quest/m1/rendition-failure.md) - moves a recovered rendition to a new track name under a stable ID, which these exporters must follow
- [Catalog broadcast references in MSF and GStreamer](/quest/m2/catalog-broadcast-reference.md) - other readers that mishandle a rendition's `broadcast`
