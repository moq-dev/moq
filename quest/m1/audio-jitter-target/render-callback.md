# [M] The browser's target counts its render callback

## Goal

The browser's audio target includes the audio its render callback pulls at
once, and its clock is paired on the wall clock, so the browser rests on the
shared clock like native does, without the underruns that pairing alone
caused.

## Plan

Decided 2026-11-11 (maintainer, /quest-merge pick of
[moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507)): keep.

Headless Chromium renders 2048-frame callbacks (`baseLatency` 42.7 ms), and
the clock is paired with `currentTime` just after each callback, so the
browser rests 28 to 35 ms behind the clock where native rests within a few
ms. A max-filtered wall-clock pairing put audio on the clock (p50 -1.3 to
4.5 ms) but underran near-zero Opus 45 times a minute, since the target did
not count the callback.

Recommendations, in order and in one PR:

1. Add the render quantum (`baseLatency`, or the measured callback size) to
   the browser ring's target.
2. Then pair the clock on the wall clock with the max filter.

Measure the browser live rows before and after; native is unchanged.

## Required

- [Calibrate](/quest/m1/audio-jitter-target/calibrate.md) - the browser ceilings this is measured against
