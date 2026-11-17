# [S] moq-c's video rate follower runs off the session runtime

## Goal

A moq-c video encoder that follows a bandwidth allocator never blocks the
`moq-c` runtime thread, which drives every session's I/O, timers, and C
callbacks: a bitrate change, a rung flush, or a hardware encoder reopen
stalls only the encoder.

## Plan

Found by the 2026-11-17 pre-cut correctness audit; confirmed from source.
`follow_reservation` in `rs/moq-c/src/video.rs` runs through
`ffi::Background` on the single current-thread `RUNTIME`
(`rs/moq-c/src/ffi.rs`) and waits there on the encoder lock (held by a C
thread for a whole encode) and on codec round trips. moq-ffi already runs
the same loop on its own thread (`spawn_follow` in `rs/moq-ffi/src/video.rs`)
for exactly this reason. `release` blocked on bitrate changes only; the
flush and reopen came with #6781.

Recommended: copy moq-ffi's dedicated-thread follower. Measure before and
after: step the bandwidth across a rung threshold and record another
subscription's frame delivery gaps.

Public API: none. Wire: none.

Caused by, for the fix PR's `## Caused by`:

- [moq-dev/moq#6781](https://github.com/moq-dev/moq/pull/6781) - added rung flush and encoder reopen to the follower on the shared runtime
