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

Public API: none, unless the maintainer picks the dial hook. Wire: none.

## Waiting

- Maintainer: how mesh tests prove convergence. (1) Recommended: give
  `cluster::Options` a dial hook that an in-memory link can fill, so these tests
  run on `kio::clock::sim` and end on `sim::idle`; this adds public API whose only
  consumer is tests. (2) Keep these windows as documented real-clock exceptions
  and opt them out of the [Rust guard](/quest/m0/clock-guard/rust.md). Check: a
  recorded pick in this file's Plan.
