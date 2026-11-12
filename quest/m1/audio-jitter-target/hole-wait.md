# [M] Split the consumer's hole wait from its age limit

## Goal

The container consumer waits on a missing group for `delay + lookahead` but
keeps delivering aged media up to the subscription's max delay, in `js/hang`
and Rust alike, so a reordered join backlog no longer raises the jitter
target while a stall's late audio is still delivered for the stretch to
catch up on.

## Plan

Decided 2026-11-11 (maintainer, /quest-merge pick of
[moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507)): keep.

#8507 set the browser consumer's single limit to the subscription's max
delay, as native had, because at `delay + lookahead` the age skip dropped
what the stretch would catch up on (1651.8 tap underruns a minute on
relay-bbb, 2123.6 on relay-mic-stall). The cost: relay-mic-rtt40's consumer
waits up to the max on its reordered join backlog's holes, which the
estimator counts as jitter, raising its target p95 from 320 to 780 ms.

Recommendations:

- Two limits: skip a hole after `delay + lookahead`, drop media only once it
  is older than the max.
- Change both consumers in one PR and keep their tests mirrored.
- Pass when relay-mic-rtt40's target p95 returns near 320 ms and the stall
  rows keep #8507's underruns and trims.
