# [S] moq-bench: a lagged group ends the subscription, so offered load decays

## Goal

`moq-bench` holds the configured subscriber count for the whole run. A group
the relay gives up on costs that group and nothing else, so a relay that
struggles is measured under the load it was asked to carry rather than under
the subscribers it managed to keep.

## Plan

The relay signals a subscriber that fell behind by failing the group with
`Error::Lagged`. In `rs/moq-bench/src/connection.rs`, `drain` propagates the
inner `group.read_frame().await?` out of both loops, `spawn_drain` logs the
error at debug, and nothing resubscribes, so that connection is out of the
run for good. On a 1:200 video fan-out the subscription gauge falls from 200 to
about 150 within a minute, and the decay is load-dependent, which flatters the
relay on every per-connection metric.

A real player skips the group it fell behind on and keeps watching. The track
itself stays open at the moq-net level; only the bench's `?` conflates the
two.

- Treat a group-level read error as the end of that group: log it at debug
  with the sequence, count it in the gap stats, and continue with the next
  `recv_group`. The outer error (track or session gone) stays terminal.
- Test: a drain fed a group that fails mid-way keeps consuming later groups.
- Re-run the 1:200 matrix from the issue and note in the PR that the gauge
  holds at the configured count.

## Closes

- [#3123](https://github.com/moq-dev/moq/issues/3123) - close this issue when the quest finishes
