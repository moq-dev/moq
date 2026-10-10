# [M] Viewer lag histogram

## Goal

The relay's `moq-stats` egress side reports, per broadcast, how far behind
its viewers are according to what the network has acknowledged: a
byte-weighted cumulative `lag` histogram and `dropped` counters (duration,
bytes, and groups dropped before they were acknowledged), on the totals and
prefix tracks of [stats totals and prefix
tracks](/quest/m0/broadcast-epoch/stats-split.md). A spliced subscription
stays exact across a route switch, and a closing subscription records its
last partial interval.

## Plan

Decided 2026-10-08: split out of the QoS line so the dashboard and publisher
timeliness can require a histogram that exists on `main`. This quest lands it
on `main` once stats-split has, moved from the per-path `publisher.json` and
`subscriber.json` map rows it writes today onto stats-split's totals and
prefix tracks.

Decided 2026-10-09: the QoS line
([#4133](https://github.com/moq-dev/moq/pull/4133)) closed without landing,
since every code change it had over `main` is this histogram. Its code is kept
on `wip/4133-lag-histogram` as this quest's starting point: the sampler
(#4298, `rs/moq-net/src/stats.rs`), the splice fixes (#5009), and the final
sample (#4451). Porting it is not mechanical:

- `main` moved to caller-supplied time (#4437) and removed the frozen test
  clock (`model::clock::advance`). The sampler reads wall time in
  `Registry::report`, when a `Frontier` opens, on `Delivery` acks, and in the
  final sample in `Drop for FrontierInner`, which has no caller to supply a
  time. Redesign those to take time from their callers, and give the final
  sample an explicit close path.
- Untimed tracks (#4822): record `Production` only for timed frames.
- `max_age` became `max_delay` on subscriptions (#4917).

Open review findings on #4133 to fix while porting:

- A timestamp regression hides a stalled viewer
  ([3](https://github.com/moq-dev/moq/pull/4133#issuecomment-6053929743)).
- A copy served across a no-route gap stays watched
  ([B](https://github.com/moq-dev/moq/pull/4133#issuecomment-6053929743)).
- A partial write is charged the frame's full declared size
  ([P2](https://github.com/moq-dev/moq/pull/4133#pullrequestreview-5452586850)).
- `Dropped::add` should saturate instead of overflowing
  ([comment](https://github.com/moq-dev/moq/pull/4133#discussion_r4215691174)).
- Bound the frontier list when `Registry::report` is not called
  ([comment](https://github.com/moq-dev/moq/pull/4133#discussion_r4215691184)).

Keep the histogram contract the dashboard reads: buckets keyed by upper edge
(`"50ms"` to `"5s"`, then `"inf"`, empty buckets omitted), cumulative and
monotonic so readers diff two samples and sum nodes bucket by bucket. Update
`doc/concept/stats.md` and the stats section of `doc/bin/relay/config.md`.

Public API: breaking in `moq-stats`, as the line already decided. Wire: the
stats JSON gains the egress fields.

## Required

- [Stats totals and prefix tracks](/quest/m0/broadcast-epoch/stats-split.md) - the totals and prefix tracks the histogram lands on
