# [S] Import discontinuity

## Goal

A publisher that seeks or pauses tells its importer so, and the flush jitter
measurement restarts instead of counting the break. `moqsink` does it on a
seek, and C and FFI publishers can do it too.

## Plan

- `moq_mux::container::Producer::discontinuity()` already resets the flush
  baseline (`catalog/estimate.rs`). `import::Track` gains a `discontinuity()`
  that forwards to it, and the codec importers without one gain it too. Only
  the baseline resets; advertised values are never lowered.
- `moqsink` calls it when a pad re-anchors after a flush or a new segment
  (`rs/moq-gst/src/sink/pad.rs`). Test that a seek on an `encoder` pad does
  not raise the advertised jitter.
- Expose it as `moq_publish_media_discontinuity` in libmoq and as
  `discontinuity()` in moq-ffi and its hand-written wrappers, per the
  Cross-Package Sync table. This is additive, so it lands on `main`.
