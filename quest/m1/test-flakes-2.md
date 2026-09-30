# [S] More tests hold up under load

## Goal

A second round after [#4286](https://github.com/moq-dev/moq/pull/4286):
tests that pass alone but have failed under a loaded `just check` pass
reliably, each fixed at its cause, never by raising a timeout or adding a
retry.

- moq-cli `fetch::tests::a_frame_read_times_out` asserts on a 500 ms wall
  deadline ([#4084](https://github.com/moq-dev/moq/pull/4084)). The cause
  (#4431): `rs/moq-cli/src/fetch.rs` wraps the whole run in one
  `timeout_at(deadline, ...)`, so connect, TLS, announce, and subscribe share
  the budget with the read, and under load setup alone can spend it.
- moq-cli `complete::tests::a_stage_broadcast_picks_the_catalog_to_read`
  ([#4084](https://github.com/moq-dev/moq/pull/4084)),
  `the_catalog_format_on_the_line_is_honored`
  ([#4089](https://github.com/moq-dev/moq/pull/4089)), and
  `a_relay_on_the_line_answers_broadcast`: both of the latter pair came back
  empty after the fixed 1.5 s `CEILING` on a loaded runner
  ([#4404](https://github.com/moq-dev/moq/pull/4404)).
- moq-net `model::group::test::drop_unfinished_warns` counts WARNs through a
  global tracing capture, so another test's WARN, or a missed one, changes
  the count ([#4104](https://github.com/moq-dev/moq/pull/4104)). The
  `model::track` test of the same name uses the same helper.
- moq-tokio `broadcast_race_quic_wins` binds TCP `:0` and then UDP on the
  same number, which nothing reserves: the collision
  [#4084](https://github.com/moq-dev/moq/pull/4084) removed from its sibling
  after [#4055](https://github.com/moq-dev/moq/pull/4055) papered over it with
  a retry.
- moq-tokio
  `subscription_end_integrity::a_subscription_cut_by_the_publisher_disconnecting_does_not_end_clean`
  ends `Ok(None)` with 10 of 20 frames in 3 of 8 full-suite runs on a clean
  tree, and passes alone (#4332).
- `just test media` late join failed once after
  [#4181](https://github.com/moq-dev/moq/pull/4181): "joined at frame 111, 16
  frames behind 127", against a budget of one GOP (15).
- js/publish audio encoder test "a rendition trailing the broadcast's
  earliest advertises delay" reads the real clock, so it fails when its file
  runs alone (timestamps go negative early in the process) (#4414). Mock time.
- moq-mux `container::ts::export_test::debounce_opens_without_a_media_clock`
  died with SIGTERM once in a combined run. It is marked
  `start_paused = true` but sleeps 1.2 s of real time, because the debounce
  reads `crate::Clock`, which uses `std::time::Instant`, so the paused tokio
  clock never reaches it.
- moq-tokio websocket `fixed_addresses_keep_tls_name_and_request_host` and
  `ipv6_literal_fixed_addresses` pause the clock over a real TLS dial on
  loopback. They pass today, but once a timeout lands on that path, the paused
  clock can fire it before loopback delivers, the race
  [#4527](https://github.com/moq-dev/moq/pull/4527) removed from the auth
  outage tests.

## Plan

- Timing tests: prefer a paused clock over wall time (`moq-cli`'s
  subscribe tests already use `#[tokio::test(start_paused = true)]`), or
  assert on an event instead of a deadline. If a test is slow under load
  because the code under test is slow, fix that.
- Fetch timeout: test on a paused clock (maintainer decision, 2026-09-28).
  The fixture runs real sockets against an in-process relay, where a paused
  clock fires QUIC timers while packets are in flight, so first make the
  timers mockable: run the fixture over an in-memory transport, or drive
  noq's timers from the test clock, whichever is smaller. Keep the one
  absolute 30 s deadline, matching the relay's `/fetch`; on a paused clock
  setup costs no time, so it can't spend the read's budget.
- moq-mux debounce: let the test drive `crate::Clock`'s time (a tokio
  `Instant` under test, or an injected source) so the window passes on the
  paused clock, and drop the real sleep.
- WARN counting: capture per test (a scoped subscriber or a filter on the
  test's own span) instead of a process-global count.
- The race test shares one port only so both transports sit behind one URL.
  Separate `:0` ports fix the bind collision but not the race itself: with
  `websocket.delay = 0` either arm can legitimately win under load. Make the
  order deterministic the way #4084 did, holding the WebSocket arm until QUIC
  has connected, rather than asserting on a real race; no retry. #4084's follow-ups
  (`tests/reconnect.rs` `spawn_server`, `tests/worker.rs` `free_udp_port`)
  are the same probe-and-rebind pattern; fix them here if cheap.
- Media late join: first decide whether 16 frames is a real regression (the
  player joining at the previous GOP's keyframe) or an off-by-one in how the
  fixture samples the live edge. Fix whichever it is; don't widen the
  budget without a reason.
- Prove it by running `just check --all` several times on a loaded machine,
  as the first round did.

Public API: none. Wire: none.
