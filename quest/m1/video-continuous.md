# [M] Rust video delivery continuity

## Goal

`moq_video::decode::Consumer::read` returns `container::Delivery<Frame>`, so
each decoded picture says whether it continues the previous delivery, like
the container and audio reads already do. A picture decoded from input before
a marker, skip, or subscribe keeps that input's flag even when the codec
returns it late, and the first picture after the boundary reads
non-continuous.

## Plan

Split from the container and audio work in
[#5288](https://github.com/moq-dev/moq/pull/5288), where the maintainer chose
the `container::Delivery<T> { frame, continuous }` wrapper and landing video
separately.

A decoder can return a picture from before a marker while processing the
access unit after it, or only on flush, so a single pending boolean attaches
the boundary to the wrong picture. Carry a private input identity through
every backend (`rs/moq-video/src/decode/backend`) and map each output back to
its input's flag. A map keyed only by presentation time is not enough: equal
timestamps can straddle a marker. VideoToolbox and Media Foundation already
map synthetic sample times, OpenH264 queues presentation times, and VAAPI,
NVDEC and MediaCodec carry codec timestamps. An output whose input identity
did not survive (Media Foundation's unmatched-output fallback) is an explicit
error, never a borrowed flag. Keep dropped-input bookkeeping bounded.

Tests: delayed output on both sides of a marker, duplicate timestamps, and
decoder recovery, on each backend CI can run.

Public API: breaking in moq-video (the read result). Bindings keep their
existing frame records. Wire: none.

## Related

- [Open-GOP leading pictures](/quest/m1/open-gop-leading-pictures.md) - the Rust decode trim keys on this signal
- [Consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) - withholds frames after the same signal
