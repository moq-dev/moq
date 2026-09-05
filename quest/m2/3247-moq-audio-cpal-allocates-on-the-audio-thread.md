# [S] moq-audio: no allocator call on the audio thread when cpal reports an error

## Goal

A cpal stream error raised from the real-time render thread reaches
`moq-audio` without an allocation or a free on that thread. Today cpal's
`From<coreaudio::Error>` runs `format!` on the render thread before our
callback sees the error, and the callback then drops the owned `Error`,
freeing on the same thread; either can block on the allocator lock and cause
a dropout, exactly when playback is already degraded.

## Plan

`FailureReporter::report` in `rs/moq-audio/src/playback/driver.rs` is already
allocation-free (atomics and `Thread::unpark`). What remains is upstream and
the hand-off:

- Raise it with cpal: allocation-free error emission on the RT paths (a
  `&'static str` message, or deferring the `format!` to a non-RT thread),
  which is where the dominant allocation lives. Carry the upstream PR as
  part of this quest and pin the release that includes it.
- Pre-check in `moq-audio` what can be checked before `stream.play()`, so
  fewer errors are raised from the render path at all.
- Document the residual constraint beside the driver: the callback owns the
  `Error`, so the overflow path still drops it on the callback thread, and no
  bounded hand-off removes that entirely.

Test: an allocation-counting harness around the error callback, the same
shape the capture-callback quest used, proving no allocation or free after
construction on the paths this repo controls.

## Closes

- [#3247](https://github.com/moq-dev/moq/issues/3247) - close this issue when the quest finishes
