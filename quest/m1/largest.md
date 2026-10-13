# [M] Live largest location

## Goal

A track reader learns the track's largest location before the first group
arrives and follows it live. `largest()` replaces `latest()` on the track
producer, consumer and subscriber in Rust and `@moq/net`, returning a
`Location` (group and frame). It is seeded by the publisher's reported
largest (moq-transport SUBSCRIBE_OK, lite-07 SUBSCRIBE_START) and advances as
frames arrive or a newer value is reported. It never lives on
`Info`, which is per-track and immutable.

## Plan

Decided with the maintainer (2026-10-13), after moq-dev/moq#5369 proposed
`Info.publisherLargest` as a snapshot of SUBSCRIBE_OK:

- **Stream only, no snapshot.** Rust: `async fn largest(&mut self)` and
  `poll_largest(waiter)`. JS: `async largest()`. The first call yields the
  current location at once if known; each later call waits for a newer one,
  skipping values in between; it ends (`None`/`undefined`) when the track
  ends.
- **An empty group is not reported.** A group that has started but has no
  frames yet leaves `largest()` at the previous real frame; a `Location`
  names a frame, and the empty group has none yet.
- **One name everywhere.** Rename on the producer, consumer and subscriber,
  and update every caller (moq-json, moq-flate, moq-mux, moq-relay, moq-hls,
  moq-c, and the rest) in the same PR. JS's buffered-only `largest()` takes
  the new meaning. Producers that pick their next sequence from
  `latest() + 1` (moq-json, moq-mux) read the sequence high-water, which
  datagrams and frameless groups also advance, so a frame `Location` can't
  replace it: move each to the track's own allocator (`append_group`). If a
  caller can't (moq-mux skips a sequence on a data-track discontinuity) or
  truly needs a synchronous read, ask the maintainer rather than adding a
  snapshot.
- Check that no cursor logic assumes the largest location was received
  locally: a reported largest is a hint about the edge, not a cached group.
- Bindings follow the cross-package table if any expose `latest()`.
- Cover on mock time in both languages: known before the first group from
  SUBSCRIBE_OK and SUBSCRIBE_START, advancing per frame, holding through a
  started group with no frames, coalescing, and
  ending with the track.
