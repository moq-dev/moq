# [M] Fix native audio/video delegate wiring, delay drain, and hardware codec UB/leaks

## Goal

macOS ScreenCaptureKit audio capture registers its delegate and validates ASBD
format/alignment, `moq-audio` drains held sync delay on `Port::drop`,
VideoToolbox sessions invalidate before dropping their callback sink, V4L2 and
MediaCodec reclaim buffers on error paths, and MediaFoundation / `CVPixelBuffer`
guards prevent null/0-len UB and leaked locks.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place high-severity and impactful UB/leak bugs in `m1`
(rejected: `m0` with `release` backports; `m3`).

1. **`rs/moq-audio/src/capture/screencapture.rs:106-117,248-262`**:
   - Pass `Some(ProtocolObject::from_ref(&*delegate))` (instead of `None`) to
     `SCStream::initWithFilter_configuration_delegate` so stream stop/error
     delegate callbacks actually fire.
   - Validate that the incoming ASBD is 32-bit float and properly aligned
     before casting raw bytes to `&[f32]` in `samples()`.
2. **`rs/moq-audio/src/bus.rs:1244-1249`**:
   - Drain `Lane::delay` on `Port::drop` (`Command::Finish`) instead of
     silently discarding buffered sync-delay audio at end-of-stream.
3. **`rs/moq-video/src/{decode,encode}/backend/videotoolbox.rs`**:
   - Implement `Drop` invalidation (`wait_for_asynchronous_frames` /
     `complete_frames` + `invalidate`) *before* `sink: Box<Sink>` drops so
     in-flight VideoToolbox callbacks cannot dereference a freed `Sink`
     (use-after-free).
   - Invalidate partially constructed sessions on early error paths in encoder
     `open()`.
4. **`rs/moq-video/src/{decode,encode}/backend/{v4l2,mediacodec}.rs`**:
   - Reclaim V4L2 `OUTPUT` buffer indices on encode/decode error paths and
     re-queue `CAPTURE` buffers before propagating `read()` errors.
   - Queue a 0-length buffer back to `AMediaCodec` when rejecting an oversized
     input so the dequeued input buffer index is not permanently leaked.
5. **`rs/moq-video/src/{decode,encode,capture}/backend/mediafoundation.rs` &
   `rs/moq-video/src/frame.rs:1781-1800,2038-2056`**:
   - Guard null pointers and zero lengths returned by `IMFMediaBuffer::Lock`
     and use `u32::try_from` for buffer lengths instead of truncating casts.
   - Check `CVPixelBufferGetBaseAddressOfPlane` for null and validate plane
     stride, and use an `UnlockGuard` RAII helper in `upload_i420` so early
     errors never leak the pixel-buffer lock.

Public API: none. Wire: none. Add unit tests for `bus.rs` delay drain, ASBD
validation, and error-path cleanup where portable; compile-check macOS/Windows/
Android/Linux backends.
