# [M] Encoders keyframe by time, not frame count

## Goal

A video encoder's groups stay within its declared join interval
(`encode::Config::join`) when the source runs slower or faster than the
configured `framerate`, such as a camera dropping to 20 fps in low light. Today
a 2 s join at 30 fps keyframes every 60 frames, which a 20 fps source
stretches to 3 s, so moq-mux warns that the track's groups are longer than the
declared interval and HLS lists them as overruns.

## Plan

Found by moq-dev/studio's first Mac capture run
([moq-dev/studio#70](https://github.com/moq-dev/studio/pull/70)). Decided
2026-11-07 (maintainer, moq-dev/studio's /quest-unblock): ✅ keyframe by
elapsed time (rejected: have AVFoundation report the device's frame rate,
which stays 30 when auto-exposure slows the camera; drop it); ✅ m1 (rejected:
m3), since it changes no public API and fixes every variable-rate source.

`Config::keyframe_interval()` (`rs/moq-video/src/encode/encoder.rs`,
crate-private) converts `join` to frames at the configured rate, and each
backend sets that as its GOP length. Instead, force a keyframe on the first
frame whose timestamp reaches the last keyframe's plus `join`, through the
forced-keyframe path that `Cuts` (`rs/moq-video/src/encode/cuts.rs`) already
drives on every backend. VideoToolbox can also take
`kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration`. Decide in the PR
whether a backend's own GOP stays as a ceiling beyond `join` or goes.

Test with a mocked source whose frame rate varies: no group exceeds `join` by
more than one frame.
