# [S] The MediaCodec encoder encodes again after a flush

## Goal

A MediaCodec encoder that drained its tail in `flush` encodes the frames fed after it, on the
AOSP software encoders as well as hardware ones. `Encoder::flush` documents flushing at a group
boundary and carrying on, and `moq_video_encoder_flush` promises "the next picture starts new
input".

## Plan

Decided 2026-11-28 (maintainer): after a flush that drained the encoder's tail with an end of
stream, recreate the codec (a new configured and started instance instead of `AMediaCodec_flush`).
Rejected: recreate only software encoders (naming the codec needs API 28, above this crate's API
26); refuse input after such a flush (breaks the documented flush-and-continue contract).

Found by [moq-dev/moq#9814](https://github.com/moq-dev/moq/pull/9814). On the API 34 emulator's
`c2.android.avc.encoder`, every frame after a flush that went through `drain_tail` (end of input,
then `AMediaCodec_flush`) vanishes: 6 frames in, only an end-of-stream buffer out, every run
(60 of 60). In-tree, `flush` with frames in flight is terminal (track finish, the probe), so
published tracks don't hit it; libmoq callers and `Encoder::flush` users who keep encoding do.

Cause, from AOSP `media/codec2/components/avc/C2SoftAvcEnc.cpp`: an end-of-stream input sets
`mSawInputEOS`, and nothing clears it. `onFlush_sm` and `onStop` are no-ops and `onReset` leaves
it set, so every later work item is encoded as the last one. Only a new codec instance
recovers. Hardware encoders are expected to resume after a flush, as the NDK documents.

Test: extend `encodes_a_keyframe_with_parameter_sets_inline` (or add a sibling) to encode one
frame, flush, then encode one more frame with a cut and flush, asserting each group opens with a
keyframe carrying its parameter sets. It fails on the emulator today.

The first frame on a codec that just started or flushed no longer drops ([moq-dev/moq#9814](https://github.com/moq-dev/moq/pull/9814) waits for an
input buffer when nothing is in flight), so a fresh instance needs nothing more for its first
frame.
