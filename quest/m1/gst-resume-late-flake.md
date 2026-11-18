# [S] moq-gst's resume test holds under load

## Goal

`source::imp::session_tests::a_closed_publisher_session_resumes_on_the_next_start`
in `rs/moq-gst/src/source/imp.rs` passes however loaded the machine is.

## Plan

It failed once under a full `just check` on 2026-11-17 with "the sink dropped
1 as late: 7 of 10 rendered", and passed three runs alone, on a branch that
touched no moq-gst code ([moq-dev/moq#9045](https://github.com/moq-dev/moq/pull/9045)).
A sink judging lateness by the wall clock fails when the machine is busy.
Reproduce it under load first, then fix the root cause, such as a test clock.
Keep the lateness check: `rendered` asserts every frame arrives on time, so a
sink that stops dropping late buffers must keep a separate lateness assertion.
No retry or looser assertion.
