# [M] Catalog track identity

## Goal

A track's catalog identity never changes for its name, so live and recorded
playback never guess which configuration applies to a group. Identity is the
codec and its description, plus anything else a decoder must be configured
with (audio sample rate and channel count). `codedWidth`, `codedHeight`,
`bitrate`, and `framerate` are ceilings fixed when the track is created:
resolution changes in band below them, and the codec string advertises the
level the ceiling needs. A change to identity, or past a ceiling, publishes a
new rendition name or a new broadcast epoch instead of mutating the track.
Live state (`enabled`, the jitter and delay figures, `warmup`) changes freely
under the same name, as do the broadcast-level display properties
(`display`, `rotation`, `flip`), which describe presentation, not decoding.

## Plan

Decided in the 2026-09-30 audit: track identity is immutable. Mutable
definitions with a configuration identity that groups reference (the old
option 2) are out, because they contradict the rule that a track name always
means the same content, and MoQ has no ETag-style invalidation.

Decided 2026-10-08: identity versus ceilings versus live state, as in the
Goal, and moved to m1 ahead of the archive line, since a replayed catalog
relies on it. Reasons:

- Resolution must change without a new name: VP8, VP9, and AV1 keyframes
  carry their size, and H.264/H.265 carry it in in-band parameter sets
  (avc3/hev1). A `description` holding parameter sets pins the resolution, so
  that track changes it only by minting a new identity. CMAF keeps its
  exception (`doc/concept/hang.md`): an avc3/hev1 CMAF track keeps a
  configuration record with no parameter sets as its `description` for the
  NAL length size, which is framing, not resolution.
- Ceilings let a decoder be configured once for the largest picture, so a
  smaller one never needs a catalog update.
- A change to codec or description, or past a ceiling, mints a new rendition
  name through a [catalog rendition ID](/quest/m1/catalog-track-id.md) when
  the catalog can keep both, or a new
  [broadcast epoch](/quest/m0/broadcast-epoch/README.md) when the whole
  broadcast restarts. The catalog may still add and remove tracks; a removed
  name is never reused for different content.

Known violation: the JS publisher rewrites `codedWidth` and `codedHeight`
when the capture resizes (`#runCatalog` in `js/publish/src/video/encoder.ts`).
Fix it to publish the ceiling and resize in band. Audit the other Rust and JS
publishers for fields that change mid-track today the same way.

Name the identity, ceiling, and live-state split in the hang draft
(`drafts/draft-lcurley-moq-hang.md`, `just drafts check`) and the catalog
docs, so a catalog update that changes only live state is never mistaken for
a new identity.

Cover codec changes, resolution changes below and past the ceiling,
rendition switches, reconnects, and late joiners in Rust, JS, and HLS/watch
tests.

Public API: the catalog contract tightens; no type changes expected. Wire:
the hang catalog's field semantics, documented in its draft.

## Related

- [Catalog rendition IDs](/quest/m1/catalog-track-id.md) - lets a catalog list a new track name for a changed rendition
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - a restart is a new epoch rather than a changed track
- [Archive](/quest/m1/archive/README.md) - storage and replay consume the identity contract
