# [S] One bad rendition never leaves the player with nothing

## Goal

A malformed audio rendition is skipped like an unsupported one, leaving its
siblings available, and conflicting video selection filters fall back to a
rendition instead of a black screen.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred,
inherited.

- `js/watch/src/audio/source.ts:94-115` probes each rendition with
  `AudioDecoder.isConfigSupported` inside `try/finally` with no `catch`. A
  rendition with `codec: ""` (the schema allows it) rejects with TypeError,
  the spawn fails, `available` is never published, and a stale previous
  value survives. Catch per rendition as `video/source.ts:278` does, and
  consider sharing one probe loop with video's support cache (audio
  re-probes every rendition on every catalog update).
- `video/source.ts:339` and `:384`: the element always sets a dimension
  target and the probe a bitrate target; `byBitrate` drops renditions
  without `bitrate` and `byDimensions` those without coded size, so with
  partial metadata the rankings can share no name and `#select` returns
  undefined, clearing the frame. On conflict fall back to the first ranking's
  best rendition, never to nothing.

Public API: none. Wire: none.
