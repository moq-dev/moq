# [M] One Annex-B exporter for H.264 and H.265

## Goal

moq-mux's H.264 and H.265 Annex-B exporters share one private lifecycle:
catalog updates, track selection, playhead tracking, keyframe joining,
parameter-set injection, payload conversion and completion. Only codec
configuration parsing and parameter sets stay per codec, and the public
codec wrappers keep their API. The shared lifecycle follows a rendition
whose resolved track or broadcast changes under the same ID.

## Plan

Found by the 2026-10-22 refactor audit (main at d44813cb7):
`rs/moq-mux/src/codec/h264/export.rs` and `h265/export.rs` are 433 and 432
lines, their `poll_next` is identical, and only 171 diff lines differ once
the codec names are normalized.

Decided 2026-10-22 (maintainer): land after the rendition rebind work,
which reworks the `ExportSource` these exporters use. Then make the shared
lifecycle follow a rebound rendition too, as that quest does for fMP4, TS,
MKV and FLV: reproduce the gap first with a test, and fix it once in the
shared code. Do not build a framework covering every container and codec.

Tests cover configuration changes, rendition switching, parameter-set
injection, keyframe joining, completion, and the rebind.

Public API: none. Wire: none.
