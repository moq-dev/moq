# [S] Hardware decoders preserve sub-microsecond frame timestamps

## Goal

NVDEC, VAAPI, and MediaCodec decode backends in `moq-video` preserve exact input `Timestamp` values without microsecond truncation, matching V4L2, so transcoded frames on 90 kHz or nanosecond timescales never fall below their `group.start`.

## Plan

`rs/moq-video/src/decode/backend/nvdec.rs`, `vaapi.rs`, and `mediacodec.rs` convert frame timestamps through `as_micros()` / `from_micros()`, discarding sub-microsecond ticks while `v4l2.rs` already preserves exact `Timestamp` values through a submitted-timestamp lookup (`restore`). When a downstream track inherits a 90 kHz or `Timescale::NANO` timescale, truncating frame timestamps can place a decoded frame below its `group.start`.

Scope:
- Preserve exact `Timestamp` values across `nvdec.rs`, `vaapi.rs`, and `mediacodec.rs` using an in-flight sideband map keyed by codec PTS/token (following `v4l2.rs`'s `restore` pattern) so reordered output pictures recover their exact input `Timestamp`.
- Add a decoder conformance test in `rs/moq-video/src/decode/conformance.rs` (and unit tests for each backend's timestamp restore helper) asserting exact sub-microsecond `Timestamp` round-trip (for example, 90 kHz tick values not divisible by microseconds).

Public API / wire: none.
