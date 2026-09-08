# [M] moq-mux: catalog jitter is the publisher's maximum flush span, never a running minimum

## Goal

Every importer advertises `jitter` with the meaning the hang draft gives it:
the maximum delay between a frame being ready and the publisher flushing it,
measured from the source's own structure (fragment duration, PES packing,
B-frame reordering) and never from arrival timing. The advertised value only
ever grows over the life of a stream, is a whole number of milliseconds
rounded up, and is never zero. One estimator computes it for every container
and codec; no importer writes the catalog field directly.

## Plan

What the tree does today (`rs/moq-mux`):

- `container/fmp4/import.rs` computes a per-fragment span
  `max - min + min_duration` and writes it straight into
  `catalog.audio.renditions[..].jitter` only when it is *smaller* than the
  previous value. A single-sample fragment yields one frame duration; two
  samples with equal timestamps make `min_duration` zero, after which a later
  single-sample fragment publishes `0` for the rest of the stream. The public
  `bbb.hang` advertises 0 for both renditions from the start. Video gets the
  same treatment.
- `container/ts/import.rs` `AacStream::write` cuts every ADTS frame of one PES
  into its own group in one synchronous pass, so a seven-frame PES arrives as
  one 162 ms burst. Its running-maximum burst span goes through
  `Rendition::update`, which does not touch the estimator's `published`
  bookkeeping, so the next bitrate refinement in `Rendition::estimate` calls
  `set_estimate` and overwrites `jitter` with one frame duration. After the
  last refinement the catalog says 23 ms for a stream flushed in 162 ms
  bursts.
- `catalog/estimate.rs` `Jitter::current()` is `max(min_duration,
  max_reorder)`; nothing measures flush span. `hang`'s `AudioConfig::jitter`
  serializes as truncated integer milliseconds, so sub-millisecond values
  reach the wire as 0.

The work:

- `Jitter` gains a flush-span observation: for each synchronous publish burst
  (a fragment, a PES, a `write_frames` batch) the span from the first frame's
  timestamp to the last frame's end. `current()` becomes the lifetime maximum
  of `min_duration`, `max_reorder`, and flush span. This is the original
  publisher's structural view; wall-clock arrival never enters it.
- The fMP4 importer feeds its fragment span into the estimator and stops
  locking the catalog to write renditions itself. The TS importer feeds its
  PES span the same way and drops `update_jitter`/`update_rendition` for
  this field, so the bitrate-refinement clobber disappears by construction.
- Serialize `jitter` as the ceiling in whole milliseconds in `rs/hang` (the
  decision #3208 already took for Opus), so a real value never truncates to
  zero. Mirror the semantics line in `js/hang` and the draft text if it needs
  the word "maximum" made explicit.
- Tests: an fMP4 stream whose fragments shrink keeps the larger span; a TS
  stream with seven frames per PES advertises the burst span and keeps it
  across a bitrate refinement; a 0.5 ms value serializes as 1; B-frame
  reordering still contributes.

## Closes

- [#3479](https://github.com/moq-dev/moq/issues/3479) - close this issue when the quest finishes

## Related

- [Auto latency](/quest/m0/3477-watch-auto-latency.md) - reads this field as its codec floor
- [#3208](/quest/m1/3208-make-2-5-ms-opus-frame-durations-work-across-bindings.md) - the same ceiling rule for Opus frame durations
