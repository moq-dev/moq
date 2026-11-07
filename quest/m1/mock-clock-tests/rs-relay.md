# [M] moq-relay's mesh tests run on a mocked clock

## Goal

moq-relay's multi-relay and process-signal tests stop proving that nothing
happened by waiting out a quiet window, and stop sleeping to settle. See the
[epic](/quest/m1/mock-clock-tests/README.md) for the rules.

## Plan

The single-relay tests are done: a session that stays open is proven by the
`end` its lease reports for the client's own close, an absent broadcast by a
listing read to `Synced`, and the GOAWAY handover by a manual clock. What is
left, as of 2026-11-06:

- `cluster_unknown.rs`: `assert_unknown_publisher_stays_announced` watches for
  a flap for `SETTLE` (1s) after the frame reaches D.
- `goaway_cluster.rs`: `cluster_migrates_on_upstream_goaway` and
  `cluster_continues_a_group_split_by_goaway` end on a 500ms window with no
  retraction, and the second also on a 500ms window with no downstream GOAWAY.
- `cluster_drain.rs`: `pass` moves a manual clock in steps and sleeps `SETTLE`
  on tokio's clock after each, so real sockets catch up.
- `shutdown_signal.rs`: `an_embedder_owns_the_signals` waits `DRAIN_TIMEOUT / 2`
  for a GOAWAY that must not come.

Each crosses a mesh of relays over loopback, where no event marks convergence:
`Synced` is per hop, origin cursors coalesce across prefixes, and retractions
are held for `update_hold`. A quiet window is the only proof that fits today.
Only the simulator's `idle` proves a mesh has settled, and the cluster dials
peers only through `moq_tokio` sockets.

Tests that only wait for progress, or bound real-socket timing from below, stay
on the real clock: the `runtime_uring.rs` and listing poll loops, `web.rs`'s
`start.elapsed() >= timeout`, `bitrate_caps.rs`'s `LEAST`, and
`shutdown_signal.rs`'s drain-window lower bounds.

Decided 2026-11-06 (maintainer, on
[moq-dev/moq#8087](https://github.com/moq-dev/moq/pull/8087)):

- ✅ **A dial hook on `cluster::Options` that an in-memory link can fill**, so
  the four tests above run on `kio::clock::sim` and prove convergence with
  `sim::idle` instead of a quiet window. Not chosen: keeping them as documented
  real-clock exceptions opted out of the [Rust guard](/quest/m0/clock-guard/rust.md).

Guidance:

- The hook is public API whose only consumer today is these tests; name it by
  role (how a peer is reached), not by the test transport.
- The mesh tests build whole relays with `Relay::load` over loopback, so the
  simulator needs a relay that accepts in-memory links too, as
  `tests/integration/memory.rs` already does for one relay.
- `shutdown_signal`'s window waits on a process signal, not a mesh: its relay
  must also take that signal from a source the simulator can drive.

Public API: a new `cluster::Options` dial hook. Wire: none.
