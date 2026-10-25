# [S] Group expiry without the track lock

## Goal

A group poll decides whether its group expired without locking the track's
`TrackState`. Lands with the contended bench (`rs/kio/benches/contended.rs`)
and `just bench-profile lock` before and after, both run on a quiet host; a measured
no-win abandons it.

## Plan

- `GroupExpiry::is_expired` (`rs/moq-net/src/model/track.rs`) reads the
  track state (`self.state.read()`) on every group poll.
  That read is 32% of all mutex wait at 16 workers in both `just bench-profile lock`
  shapes, per the
  [epic](/quest/m2/perf/kio-channel-contention/README.md)'s baseline.
- Suggested direction: an atomic mirror of the verdict, like `Alive::aborted`
  (`rs/moq-net/src/model/group.rs`), written under the track lock when the
  verdict can change and read on its own by the group poll.
- Open: what the mirror holds. The verdict combines the successor's first
  frame, the drift edge, the budget, and whether the slot is still cached
  (`poll_drifted`), and the read also parks the waiter on
  landings and deadlines. Settle the shape first, with options and a
  recommendation.
- Rerun the baseline on a quiet host before the change, since the epic's
  numbers came from a loaded one.
