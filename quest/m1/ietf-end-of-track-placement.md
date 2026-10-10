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
END_OF_GROUP. Fastly's moq-relay-interop harness is external, so the PR
notes the change for their next rerun of those two cells.

Public API: none. Wire: no format change, but a peer sees END_OF_TRACK at a
different Location.

**Investigation:**

The standalone Rust marker sets END_OF_GROUP through GroupFlags::default(); a
regression test demonstrates it and this draft clears the flag, matching JS.

Exact Location preservation remains unresolved. The shared track model stores
an exclusive final **group** sequence, not the upstream terminal Object ID.
Both publishers receive this reduced boundary and currently reconstruct the
marker as `final/0`. A private snapshot of the last group served cannot recover
the upstream Location after cache eviction, when a subscription range excludes
that group, or when the end arrived before the group's data. Group streams also
FIN independently of the later track terminal, so an already finished stream
cannot accept the status.

Recommended follow-up is to preserve the terminal Location in the shared track
model, then append the status to an open final stream or use a standalone marker
at that Location after FIN. An alternative is to derive the Location from the
last served group, with explicitly narrower guarantees. Retaining only the
existing exclusive group boundary cannot meet the stated goal. The model/API
and cross-language scope should be settled before the Location portion lands;
the quest remains open.
