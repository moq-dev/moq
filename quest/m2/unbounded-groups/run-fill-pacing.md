# [M] Lite-07 history runs are paced

## Goal

A lite-07 copy bringing a long run of history to a slow reader (a range fill, a
recovery, or the reader's own history range on the copy's subscription)
backpressures the upstream once it sees a downstream reader lose a group to
eviction, so a slow reader of a long run stays within the tail instead of
falling off it.

## Plan

Added 2026-11-06 (maintainer, follow-up planning in `/quest-complete`), from
[moq-dev/moq#8017](https://github.com/moq-dev/moq/pull/8017), which paces a
batch fill's handler by its fastest reader. Pace on evidence, not up front: an
unpaced run is fine while every reader keeps up.

Landed in [moq-dev/moq#8117](https://github.com/moq-dev/moq/pull/8117): the
evidence is the track's eviction count (`TrackState::evictions`, bumped when a
reader loses a group it judged cached), and a moq-transport FETCH run holds its
fetch stream unread at each group boundary until a reader picked up the group
before (`Fill::poll_run_paced`), through the session's pause budget.

What remains is lite-07, which that PR's prototypes found harder than the plan
assumed:

- History reaches a lite-07 copy three ways, all unpaced: the copy's upstream
  subscription carrying a reader's own history range (the common case, and not a
  fill), `RangeFills` asking a held fill's run through that subscription, and a
  `Recovery` subscription asking again for what the subscription already
  accounted for. Each arrives as one group stream per group, concurrently.
- Holding each arriving group stream unread until the fill's readers picked up
  the one before admits streams in arrival order, not the fill's, so groups a
  reader takes last evict each other first. A slow reader of 40 groups on a
  4-group cache still got 10.
- Fetching a paced fill a group at a time instead (lite's `FillServeRun`, paced
  since #8017) got that reader 26 of 40, but costs a round trip per group, which
  #8017 and [moq-dev/moq#7339](https://github.com/moq-dev/moq/pull/7339)
  rejected, and paces neither the subscription's own history range nor several
  fills at once.
- #8017's per-fill pacing does not hold when one reader has several fills out,
  split by groups already cached or by refills: each fill moves on as the reader
  picks up its answer, so together they still outrun it. This affects batch
  handlers and FETCH runs too.

The mock transport moves a whole run in an instant, before any reader polls, so
a test needs the evidence first: one slow reader loses part of a run, then a
second slow reader of another long run gets all of it.

Public API: none expected. Wire: none.

## Waiting

- Maintainer: pick lite-07's pacing mechanism. (a) Hold arriving group streams
  unread and admit them in the fill's order, within the session's pause budget,
  extended to the history range a reader's subscription carries: no round trips,
  but out-of-order streams spend the pause budget and read on unpaced once it
  runs out. (b) Narrow the subscription's ranges to the next owed group once
  paced: in-order, but a SUBSCRIBE_UPDATE and a round trip per group. (c) Fetch
  paced runs a group at a time: reuses `FillServeRun`, but a round trip per group
  and the subscription's own range stays unpaced. (d) Drop lite-07 pacing and
  rely on refill-once. Recommended: (a), since it is the plan's "hold the
  upstream stream" without round trips; pair it with pacing per reader across
  its fills (a read picks up one answer per group it takes, all of them while it
  waits), so several fills of one reader cannot outrun it. An agent cannot take
  this: (b) and (c) reopen #7339's rejection, and the per-reader change rewrites
  #8017's settled pick-up rule.
