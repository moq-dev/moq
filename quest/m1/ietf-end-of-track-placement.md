# [S] END_OF_TRACK rides the upstream's Location

## Goal

When a moq-transport publisher ends a track, `rs/moq-net` and `js/net` send
END_OF_TRACK at the upstream's final Location (5/5 on group 5's stream),
rather than as 6/0 on a new stream. Rust's END_OF_TRACK stream header clears
END_OF_GROUP, as JS's already does.

## Plan

Deferred from [#5077](https://github.com/moq-dev/moq/pull/5077) (decided
2026-10-08 there as conformance polish with no lost data), from Fastly's
follow-up on #5020 (cells `status-end-of-track` and `publish-done`).
Decided 2026-10-08: both in one quest.

- Rust's `write_end_of_track` (`rs/moq-net/src/ietf/publisher.rs`) builds its
  header from `GroupFlags::default()`, which sets END_OF_GROUP; JS clears it.
- Moving onto the upstream's Location means writing the status on the last
  group's own stream when it is still open; settle what happens when that
  stream has already finished.

Test in both languages: a track ending after group 5 object 5 delivers
END_OF_TRACK at 5/5, and an END_OF_TRACK on its own stream does not claim
END_OF_GROUP.

Public API: none. Wire: none (placement and a header bit, not format).
