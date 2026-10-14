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
- Separate source state from output state. Source state (the subscription,
  group and frame cursors, the codec description) resets, because the new
  track is new content. Output state already emitted continues: TS
  continuity counters live per PID outside each `Track` for this reason,
  and fMP4's fragment sequence, declared track ID and written timestamp
  frontier belong to the ongoing output. Each muxer either signals a
  format-specific discontinuity on the continuing output, or ends the export
  loudly where its format cannot express the switch; it never resets output
  state mid-stream.
- Revalidate declarations already emitted. A replacement track can carry
  different initialization data (an AVC or HEVC SPS/PPS, geometry), which
  the fMP4 `moov`, MKV `Tracks` and FLV sequence header already declared for
  the old track. Each rebind resolves the replacement's declaration and
  compares it: if it differs, emit a new header where the format permits, or
  fail before writing any replacement payload. The tests include a
  replacement with changed initialization data.
- Tests per exporter: a rendition `main` moved from `video-a` to
  `video-b` exports `video-b`'s frames after the update and none of
  `video-a`'s, and a `broadcast`-only change does the same. Each test also
  asserts the output across the switch: TS continuity counters keep
  counting on the reused PID, fMP4 fragment sequence numbers and the
  timestamp frontier never step back, and the format's discontinuity signal
  is present, or else the export ends with an error.

Raised by a Codex review of #5367, which added the per-rendition `track`
field; the `broadcast` gap predates it. Deferred from that PR by the
maintainer's recommended option. Starts after #5367 merges.

Public API: none. Wire: none.
