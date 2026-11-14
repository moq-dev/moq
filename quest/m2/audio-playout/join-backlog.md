# [M] Both consumers report the join's backlog they skip

## Goal

The playout estimator learns when a subscription's opening backlog finished
arriving even when the container consumer skips that backlog unread, so a
join whose backlog the consumer drops no longer holds the target above the
steady state. `relay-mic-rtt40`'s replay target p95 falls from 320 ms to the
path's own, in both players.

## Plan

Found by [moq-dev/moq#8730](https://github.com/moq-dev/moq/pull/8730), which
made the estimator fold nothing for an interval holding a frame due before the
join's backlog finished arriving (`doc/concept/audio-jitter.md`, "The join's
backlog"). The estimator knows a frame is backlog from its timestamp, so it
only sees the backlog it observes. The browser consumer
(`js/hang/src/container/consumer.ts` `#run`) closes a group below its cursor
as soon as it arrives, unread, and `moq-mux`'s `container::Consumer` discards
groups below its start the same way. In `relay-mic-rtt40`, 267 groups 5 to 6 s
old arrive 1.0 to 1.3 s into the join and delay the live frames behind them by
about 300 ms. The player never observes those groups, so the delay reads as
an ordinary stall, which the cold start folds at `FRESH_MAX`: 320 ms for a 20 s
half-life. The raw trace with every frame observed holds 60 to 100 ms (the
corpus `join` case).

Recommended: when either consumer drops a group below where delivery started,
it reports that group's arrival to the estimator as backlog (a new
`Jitter.backlog(arrival)` and Rust counterpart that raises `joined`), and still
closes the group unread. A group dropped below a cursor that a later gap moved
is not reported: it is an outage's, which the stall rule owns. Add a corpus
field for frames reported this way instead of observed.

Rejected: reading and observing every frame of a group below the cursor before
closing it, as the observation point's "before any group can be skipped"
reads. That keeps stale groups open, so it spends the bandwidth closing them
saves, which for video's keyframes is large.
