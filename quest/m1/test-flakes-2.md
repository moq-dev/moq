# [S] More tests hold up under load

## Goal

A second round after [Tests hold up under load](/quest/m1/test-flakes.md):
tests that pass alone but have failed under a loaded `just check` pass
reliably, each fixed at its cause, never by raising a timeout or adding a
retry.

- moq-cli `fetch::tests::a_frame_read_times_out` asserts on a 500 ms wall
  deadline ([#4084](https://github.com/moq-dev/moq/pull/4084)).
- moq-cli `complete::tests::a_stage_broadcast_picks_the_catalog_to_read`
  ([#4084](https://github.com/moq-dev/moq/pull/4084)) and
  `the_catalog_format_on_the_line_is_honored`
  ([#4089](https://github.com/moq-dev/moq/pull/4089)).
- moq-net `model::group::test::drop_unfinished_warns` counts WARNs through a
  global tracing capture, so another test's WARN, or a missed one, changes
  the count ([#4104](https://github.com/moq-dev/moq/pull/4104)). The
  `model::track` test of the same name uses the same helper.
- moq-tokio `broadcast_race_quic_wins` binds TCP `:0` and then UDP on the
  same number, which nothing reserves: the collision
  [#4084](https://github.com/moq-dev/moq/pull/4084) removed from its sibling
  after [#4055](https://github.com/moq-dev/moq/pull/4055) papered over it with
  a retry.
- `just test media` late join failed once after
  [#4181](https://github.com/moq-dev/moq/pull/4181): "joined at frame 111, 16
  frames behind 127", against a budget of one GOP (15).

## Plan

- Timing tests: prefer a paused clock over wall time (`moq-cli`'s
  subscribe tests already use `#[tokio::test(start_paused = true)]`), or
  assert on an event instead of a deadline. If a test is slow under load
  because the code under test is slow, fix that.
- WARN counting: capture per test (a scoped subscriber or a filter on the
  test's own span) instead of a process-global count.
- The race test shares one port only so both transports sit behind one URL.
  Drive the race with each side on its own `:0` port as #4084 did, or have
  the listener reserve both itself; no retry. #4084's follow-ups
  (`tests/reconnect.rs` `spawn_server`, `tests/worker.rs` `free_udp_port`)
  are the same probe-and-rebind pattern; fix them here if cheap.
- Media late join: first decide whether 16 frames is a real regression (the
  player joining at the previous GOP's keyframe) or an off-by-one in how the
  fixture samples the live edge. Fix whichever it is; don't widen the
  budget without a reason.
- Prove it by running `just check --all` several times on a loaded machine,
  as the first round did.

Public API: none. Wire: none.

## Related

- [Archive enrollment](/quest/m1/archive/enrollment-flake.md) - the same
  kind of flake on the archive line, where its test lives
- [Interop flakes](/quest/m1/interop-flakes.md) - port reservations and the
  pause click in the interop harness
