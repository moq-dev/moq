# [M] Rust consumers hold a disabled rendition

## Goal

Every first-party Rust consumer keeps its selected rendition, and its
subscription, while the publisher has it disabled and no enabled alternative
exists, as [@moq/watch does](/quest/m1/push-to-talk/hold-js.md).

## Plan

Decided 2026-10-28 (maintainer): the hold applies in every first-party
consumer, with the same rule as the JS quest. Initial selection skips disabled
renditions, an enabled alternative wins, and a removed rendition is dropped.

- `rs/moq-mux/src/select.rs:93,121`: the selection filters require
  `config.enabled`.
- `rs/moq-cli/src/play/media.rs:192-193,445-452`: `stop_disabled` drops demand
  on the playing rendition.
- `rs/moq-ffi/src/audio.rs:613-616`: a disabled rendition returns `NotFound`.
  It should keep a held one, and the wrappers' docs follow.

[moq-audio follows the grant](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md)
already expects a held listener to keep demand on a single audio rendition
disabled below the codec floor.

The spec wording lands with whichever hold quest lands first.

Tests: the JS quest's cases, in moq-mux select and moq-ffi.

Public API: moq-ffi returns a held disabled rendition where it returned
`NotFound`. Wire: none.

## Related

- [JS consumers hold a disabled rendition](/quest/m1/push-to-talk/hold-js.md) - the same rule in JS
