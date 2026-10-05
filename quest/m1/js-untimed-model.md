# [M] @moq/net carries untimed frames faithfully

## Goal

`@moq/net` mirrors [moq-net carries untimed frames
faithfully](/quest/m1/untimed-model.md). An untimed track stays untimed from
publisher to consumer, and js/net never fills in `Timestamp.now()` on
receive. Covers `@moq/net` and its in-repo callers
(`@moq/hang`, `@moq/loc`, `@moq/watch`).

## Plan

Decided (2026-10-02): a hand-written change now, rather than
waiting for [Generated @moq/net](/quest/m1/rs2ts/README.md). That line is
long-running and would stall the JS timestamp quests. Whichever lands second
absorbs the other. The semantics, the end-marker rule and the reasons are in
the Rust quest; keep the two in step.

Decided (2026-10-05): timedness is per track, in the shape [Typed
timedness](/quest/m1/typed-timedness.md) mirrors into `@moq/net`. Where the
notes below assume a per-frame optional timestamp, that shape wins.

Things to look out for:

- Receive-side fills: lite frames on a track with no timescale, and IETF
  objects without a timestamp, both default to now today.
- The track's drift, reach, staleness and expiry logic skips unstamped groups
  the way Rust does. Check that untimed frames still count as frames, and
  that an untimed track starts at the latest group.
- A track's `Info.timescale` is required today, with a default. Like Rust,
  a track that never declared one must not claim a timeline downstream.
- A FETCH is timed only when it learns the track's units when accepted.
- Tracks on drafts 14-16, where SUBSCRIBE_OK can't carry TIMESCALE, are
  untimed (decided 2026-10-05), so the publisher sends no Timestamp there, as
  [IETF timestamp units](/quest/m1/ietf-timestamp-units.md) plans. This
  replaces the earlier plan to write an object-scope TIMESCALE beside each
  Timestamp.
- Update callers in js/hang and js/loc (end markers) and anything in
  js/watch that reads frame timestamps.

Test: an untimed frame survives a JS subscribe on each receive path. Run
`just test interop --all`.

Public API: breaking. Wire: none beyond what IETF timestamp units
changes.

## Required

- [Typed timedness](/quest/m1/typed-timedness.md) - the per-track types this mirrors

## Related

- [moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md) - the Rust side and the decisions
- [Generated @moq/net](/quest/m1/rs2ts/README.md) - will replace this code with generated code
