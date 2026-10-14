# [S] Contended kio bench

## Goal

A multi-threaded kio benchmark shows how `Producer`/`Consumer` cost grows
with threads and consumers, and a `just bench-lock` run names which moq-net
channels (track, group, broadcast, origin state) are contended. Both results
are recorded in the [epic](/quest/m2/perf/kio-channel-contention/README.md)
so the fix quests pick their targets and measure against it. No change to
kio itself.

## Plan

- Today's `rs/kio/benches/channel.rs` is single-threaded, and moq-net's
  fan-out benches (`track_fanout_group`, `viewers.rs`) run on one thread, so
  no bench measures cross-core contention.
- Add a criterion bench under `rs/kio/benches/` that runs one producer and
  consumers spread over 1, 4, and 16 threads, swept over the consumer count
  (from one up to thousands), so a cost that grows with fan-out shows up as a
  slope. Three workloads:
  - `Producer<u64>` notify cycle: write, wake all, every consumer polls and
    parks again, matching `channel_notify_cycle`.
  - Unchanged re-polls: consumers are woken or re-polled with no change to
    the value, as when another source (a deadline, a second channel) wakes
    their task. This is the path the
    [version counter](/quest/m2/perf/kio-channel-contention/version.md)
    targets, which the notify cycle never exercises.
  - A `GroupState`-shaped append log: the producer appends cheap-to-clone
    items to a `VecDeque`, and each consumer reads by cursor and clones one
    item out under the lock, as `group.rs` readers do.
- Wire it into CI the way the other benches are (at least nightly).
- Run `just bench-lock` on the chat and fanout shapes at 1, 4, and 16 tokio
  workers. Attribute the hot sites (`Consumer::poll`, `ConsumerWeak::poll`,
  `Consumer::read`) to channels by their caller frames, and record a table of
  contended channel by wait share in the epic README.

## Related

- [Relay profiling](/quest/m2/perf/lock-profile.md) - the recipe this run uses
