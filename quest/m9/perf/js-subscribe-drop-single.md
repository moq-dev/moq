# [S] A lone JS subscriber's drops cost no more than shared ones

## Goal

In `just js bench subscribe-drop`, a drop costs no more for 1 publisher and
1 subscriber than for 16 x 16, or the PR that settles this explains why it
must.

## Plan

[moq-dev/moq#8520](https://github.com/moq-dev/moq/pull/8520) measured
1190 us per drop at 1 x 1, against 570 us at 16 x 16 and 160-220 us for the
fastest rows. No earlier run gives a baseline. Profile the 1 x 1 row
(`js/net/bench/subscribe-drop.ts`) to see whether the time goes to the drop
path in `js/net/src/lite/publisher.ts`, or to session setup and warmup that
the per-drop division only spreads over fewer drops. Fix a waste the
profile shows; if it is only warmup, make the benchmark warm up before
timing.
