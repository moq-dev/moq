# [M] moq-relay's integration tests pass when they share one process

## Goal

`cargo test -p moq-relay --test integration` passes with default threads,
where every test in the binary runs as a thread of one process, as it
already does under nextest's process-per-test. Each failure is fixed at its
cause: no test-thread cap, retry, or longer deadline.

## Plan

Found while landing
[moq-dev/moq#6434](https://github.com/moq-dev/moq/pull/6434), which removed
every environment write and crypto-provider unwrap. With those gone, 9 to 11
of 118 tests still fail on a full concurrent run; each passes alone and the
whole binary passes with `--test-threads=1`. Seen 2026-10-23 on main
[69b0538326](https://github.com/moq-dev/moq/commit/69b0538326):

- `smoke::relay_websocket_logs_its_close_under_a_cluster_id` captures logs
  with a thread-local `set_default` subscriber, but tracing caches callsite
  interest process-wide, so a callsite another test registered first stays
  disabled for it. It fails whenever it shares the process.
  `runtime_uring.rs` installs a global subscriber with the same "nextest runs
  each test in its own process" assumption.
- `refusals` reads embedder metrics that look process-wide, so other tests'
  sessions change the totals it asserts (`refusals.rs` unwraps a missing
  series), and one scrape hit a refused connection.
- `cluster_unknown` saw a refused or reset connection to its HTTP endpoint
  and announce flaps across the triangle, and in
  `auth_lifetime::a_late_decision_within_the_deadline_admits` the admitted
  session closed within a second of the 11 s paused-clock jump: load or
  shared state, to be confirmed.

Find what each one shares with the rest of the process (subscriber, metrics
registry, ports, clocks) and give the test its own, or fix the code if it
holds that state where it should not.

Public API: none expected. Wire: none.
