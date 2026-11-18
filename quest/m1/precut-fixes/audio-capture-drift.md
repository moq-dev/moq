# [M] Captured audio stays in sync with video over long sessions

## Goal

Native audio capture corrects its device clock's skew against the host clock,
so a published broadcast's audio does not drift against its video over hours:
the offset after a long session stays within one audio frame of the offset at
its start.

## Plan

Found by the 2026-11-17 pre-cut correctness audit, from source; measure
first. `rs/moq-audio/src/encode/producer.rs` and `encode/capture.rs` set the
audio PTS once from the first capture time and then advance it by sample
count at the nominal rate, while video is stamped from each frame's capture
time (`rs/moq-video/src/encode/producer.rs`). A mic clock tens to hundreds of
ppm off the host accumulates as A/V offset: an estimated 0.18 to 0.36 s per
hour at 50 to 100 ppm. On `release` too.

Recommended: compare each buffer's capture time with its sample-count PTS and
correct the skew by resampling, the way the playback ring tracks its clock.
Measure with a simulated device clock offset by a fixed ppm, and on real
hardware over an hour.

Public API: none. Wire: none.
