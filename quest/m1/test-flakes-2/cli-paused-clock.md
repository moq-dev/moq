# [M] moq-cli tests on a paused clock

## Goal

The moq-cli fetch and completion tests pass under any host load: none of
them races a wall-clock budget against connect, TLS, announce, and
subscribe.

## Plan

- `fetch::tests::a_frame_read_times_out` asserts on a 500 ms wall deadline
  ([#4084](https://github.com/moq-dev/moq/pull/4084)).
  `rs/moq-cli/src/fetch.rs` wraps the whole run in one
  `timeout_at(deadline, ...)`, so setup shares the budget with the read, and
  under load setup alone can spend it.
- `complete::tests::a_stage_broadcast_picks_the_catalog_to_read`
  ([#4084](https://github.com/moq-dev/moq/pull/4084)),
  `the_catalog_format_on_the_line_is_honored`
  ([#4089](https://github.com/moq-dev/moq/pull/4089)), and
  `a_relay_on_the_line_answers_broadcast`: the latter pair came back empty
  after the fixed 1.5 s `CEILING` on a loaded runner
  ([#4404](https://github.com/moq-dev/moq/pull/4404)).

Test on a paused clock (maintainer decision, 2026-09-28). The fixture runs
real sockets against an in-process relay, where a paused clock fires QUIC
timers while packets are in flight, so first make the timers mockable: run
the fixture over an in-memory transport, or drive noq's timers from the test
clock, whichever is smaller. Keep fetch's one absolute 30 s deadline,
matching the relay's `/fetch`; on a paused clock setup costs no time, so it
can't spend the read's budget. The completion tests reuse the same fixture.
