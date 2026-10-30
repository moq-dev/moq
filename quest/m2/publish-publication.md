# [M] @moq/publish encoders keep their per-broadcast state in the publication

## Goal

`@moq/publish`'s video and audio encoders keep what they advertised on a
network broadcast, and how their registration failed, in that broadcast's
`Publication` entry instead of their own per-`Broadcast` maps and latches, so
a new network broadcast resets them structurally.

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. Internal only.

`Broadcast` already keeps each network broadcast's renditions in one
`Publication` (`js/publish/src/publication.ts`): catalog ID to kind, track
name, track, and owner, with identity rules mirroring Rust's
`hang::catalog::VideoConfig::same_identity` and `AudioConfig::same_identity`.

What remains is per-encoder state kept in agreement with it:

- `#advertised`, a WeakMap keyed by the outer `Broadcast`, holding the
  ceiling, raised bitrate and frame rate maxima, and last config. It outlives
  a network broadcast's restart (a rename or `maxAge` change) even though the
  track it describes does not.
- Three failure layers per encoder (`#failures`, the `#failure` `Once`, and
  `out.error`), bridged to the broadcast through the `errors` WeakMap in
  `js/publish/src/internal.ts`.

Public API: none. Wire: none.
