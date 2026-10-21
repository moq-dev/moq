# More tests hold up under load

## Goal

A second round after [#4286](https://github.com/moq-dev/moq/pull/4286):
tests that pass alone but have failed under a loaded `just check`, or
intermittently in CI or interop, pass reliably, each fixed at its cause,
never by raising a timeout or adding a retry.

## Plan

Decided in the 2026-09-30 audit: the round held independent flakes in one
quest, so it split into one child per flake, grouping only those that share
a fixture. Each child lands on its own.

Rules every child keeps:

- Prefer a paused clock over wall time (`moq-cli`'s subscribe tests already
  use `#[tokio::test(start_paused = true)]`), or assert on an event instead
  of a deadline. If a test is slow under load because the code under test is
  slow, fix that.
- A paused clock auto-advances while the runtime idles, so it can fire a
  timer before a real socket delivers. Where real sockets fight the paused
  clock, make the timers mockable or move the test off real sockets.

Added 2026-10-15 (maintainer): three flakes seen while landing that day's
merge batch, ranked by how likely each hides a product bug.

This README's own work, after the children: run `just check --all` several
times on a loaded machine, as the first round did. moq-tokio's
`a_subscription_cut_by_the_publisher_disconnecting_does_not_end_clean` is a
known exception, tracked by [Readers see SUBSCRIBE_DROP](/quest/m1/subscribe-drop-errors.md).

Added 2026-10-19 (maintainer): three more, the redial pair first since CI
hit it. The same day's test cost audit added the macOS QUIC connects, and the
redundant failover drift first: it may hide a TS export bug and failed two of
three loaded `just check` runs.

Added 2026-10-20 (maintainer): three more from that day's merge batch, the
draft-16 cancel race first since it fails on a mocked clock and so may hide
an ordering bug. The failed-listen test was meant to join the redial quest,
which #6032 had already finished, so it reuses #6032's reserved-port fix as
its own child. moq-uring's `host_tracks_moq_clock` was seen the same day
but #6056 had already fixed it by measuring the calibration gap.

Public API: none. Wire: none.

## Required

- [Redundant failover drift](/quest/m1/test-flakes-2/redundant-failover-drift.md) - a stitching TS export switches back to the surviving importer behind bufferbloat instead of failing on a stalled clock
- [Draft-16 cancel race](/quest/m1/test-flakes-2/draft16-cancel-race.md) - the cancel-ahead-of-the-answer test settles the same way every run, fixing `run_publish_namespaces` if the order is real
- [macOS QUIC connects](/quest/m1/test-flakes-2/macos-quic-connect.md) - moq-tokio's loopback QUIC tests stop timing out on the macOS Platform job
- [Relay bench holder](/quest/m1/test-flakes-2/relay-bench-holder.md) - the foreign-listener bench test fails fast when its holder cannot bind, instead of hanging on a fifo
- [moq-tokio failed listen](/quest/m1/test-flakes-2/tokio-failed-listen.md) - the failed-listen test keeps its port reserved from first bind to final check

## Related

- [Controlled time](/quest/m1/time/README.md) - the systemic fix these rules ask for: every crate on a controlled clock
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - tracks the media fixture's cold-start audio-tone flake, a player stall it shares with the mono pin
