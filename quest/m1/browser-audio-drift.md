# [M] Browser-captured audio stays in sync with video over long sessions

## Goal

`@moq/publish` corrects its microphone's clock skew against the clock its
video frames are stamped on, so a browser broadcast's audio does not drift
against its video over hours: the offset after a long session stays within
one audio frame of the offset at its start, as native capture's does.

## Plan

Found while fixing native capture's drift
([audio-capture-drift](https://github.com/moq-dev/moq/pull/9059)), from
source; measure first. `js/publish/src/audio/capture-worklet.ts` stamps
each render quantum by the AudioContext's running sample count from one
zero point, while a video frame carries its own capture time. The
AudioContext renders on an audio device's clock, tens of ppm off the host's,
so the offset grows by ppm × duration, as native capture's did (0.36 s per
hour at 100 ppm).

Recommended: the native fix's shape. Compare each quantum's sample-count
time with the host clock at capture (`AudioContext.getOutputTimestamp()`
or the worklet's `currentTime` against `performance.now()`), and steer the
existing JS resampler's ratio by a PI loop on the per-second minimum error,
re-anchoring a sustained lag past 100 ms. `rs/moq-audio/src/encode/drift.rs`
holds the loop and its tests.

Gate: like [ts-step-damage](/quest/m1/ts-step-damage.md), found from
source with no field report, so a standalone m1 quest outside the
[pre-cut gate](/quest/m1/precut-fixes/README.md) unless the maintainer
moves it in.

Public API: none. Wire: none.
