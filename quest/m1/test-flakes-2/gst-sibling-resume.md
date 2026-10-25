# [M] moq-gst resumes a named epoch after a sibling restart

## Goal

`a_sibling_restart_idles_a_reference_naming_the_old_epoch`
(`rs/moq-gst/src/source/imp.rs:4175`) never fails under load. Once the
instance a reference names serves its path again, the source always goes
back to reading it on the same pad.

## Plan

Decided 2026-10-25 (maintainer): treat it as a liveness bug first, then the
test. It failed once under host load during a `just check` for
[moq-dev/moq#6464](https://github.com/moq-dev/moq/pull/6464), and passed 5 of
5 times in isolation and in the full suite. The failure was
`3 never reached a pad: [Pad("video_0"), Segment("video_0", 46000), Buffer("video_0", 1)]`
at `Recorder::written`: after the replacing epoch dropped and the second
Restart arrived, the old instance wrote tag 3 every 50 ms for 10 s and none
of it reached the pad. A delay that long points to a missed resume, not a
slow test.

- Reproduce first: under load, or deterministically by forcing the order of
  the Restart and the old instance's re-subscription with a
  `moq_clock::Manual` clock.
- Find why the idle pad never reads the named instance again, and fix it in
  moq-gst or moq-net, wherever the resume is lost.
- Then make the test wait on the announce and pad events instead of the
  sleep-polled `written()`.

Separate from [gst-pinned-replay](/quest/m1/test-flakes-2/gst-pinned-replay.md)
by decision: it uses the same `Recorder` fixture, but the cause is likely
different.

Public API: none expected. Wire: none.

## Related

- [moq-gst pinned replay](/quest/m1/test-flakes-2/gst-pinned-replay.md) - the same fixture's sleep-poll race
