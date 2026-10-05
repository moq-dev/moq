# [M] @moq/net carries untimed frames faithfully

## Goal

`@moq/net` mirrors [moq-net carries untimed frames
faithfully](/quest/m1/untimed-model.md). A frame or datagram without a
timestamp stays untimed from publisher to consumer, and js/net never fills in
`Timestamp.now()` on receive. Covers `@moq/net` and its in-repo callers
(`@moq/hang`, `@moq/loc`, `@moq/watch`).

## Plan

Decided (2026-10-02): a hand-written change now, rather than
waiting for [Generated @moq/net](/quest/m1/rs2ts/README.md). That line is
long-running and would stall the JS timestamp quests. Whichever lands second
absorbs the other. The semantics, the end-marker rule and the reasons are in
the Rust quest; keep the two in step.

Things to look out for:

- Receive-side fills: lite frames on a track with no timescale, and IETF
  objects without a timestamp, both default to now today.
- The track's drift, reach, staleness and expiry logic skips unstamped groups
  the way Rust does. Check that untimed frames still count as frames, and
  that an untimed track starts at the latest group.
- A track's `Info.timescale` is required today, with a default. Like Rust,
  a track that never declared one must not claim a timeline downstream.
- A FETCH keeps timestamps whenever the track or the object gives units.
  Only objects with neither are untimed.
- On drafts 14-16, where SUBSCRIBE_OK can't carry TIMESCALE, the publisher
  writes an object-scope TIMESCALE beside each Timestamp (the `stamped` path
  in `js/net/src/ietf/publisher.ts`), as Rust does once #4822 lands. This
  replaces the JS half of [IETF timestamp
  units](/quest/m1/ietf-timestamp-units.md), which planned to send none there.
- `Frame.timestamp` and `Datagram.timestamp` become optional. Update callers
  in js/hang and js/loc (end markers) and anything in js/watch that reads
  them.

Test: an untimed frame survives a JS subscribe on each receive path. Run
`just test interop --all`.

Public API: breaking. Wire: on drafts 14-16, timed objects gain an
object-scope TIMESCALE beside the Timestamp; nothing else changes.

## Related

- [moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md) - the Rust side and the decisions
- [Generated @moq/net](/quest/m1/rs2ts/README.md) - will replace this code with generated code
