# [S] Rust non-continuous signal

## Goal

`moq_mux::container::Consumer` tells its reader whether a frame picks up
exactly where the previous one left off, like `continuous` in
`js/hang/src/container/consumer.ts`: false on the first frame after the
subscribe and after every discontinuity, true otherwise.
`rs/moq-video/src/decode` and `rs/moq-audio/src/decode` pass it through, so
the open-GOP trim, audio warmup, and consumer warmup key on one signal
instead of each adding its own.

## Plan

Split out of [Open-GOP leading pictures](/quest/m1/open-gop-leading-pictures.md)
in the 2026-10-08 audit, so audio and consumer warmup need not wait on the
open-GOP trim.

Today `poll_read` returns a bare frame, and `discontinuity()` is a counter
bumped on a declared marker group, an unproven delivered hole, or a latency
skip, but not on the subscribe itself. Report the flag with each frame and
let the counter go if nothing else needs it. Mirror the JS name.

Tests: the first frame after a subscribe, a marker group, a delivered hole,
and a latency skip each read as non-continuous, and every other frame as
continuous.

Public API: breaking in moq-mux (the consumer's read result changes). Wire:
none.

## Related

- [Consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) - withholds frames after the same signal
