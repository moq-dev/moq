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
known exception, tracked by [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md).

Added 2026-10-19 (maintainer): three more, the redial pair first since CI
hit it.

Public API: none. Wire: none.

## Required

- [moq-tokio redial tests](/quest/m1/test-flakes-2/tokio-redial.md) - the two redial tests stop assuming a dropped port stays free and stop racing 10 s deadlines
- [moq-ffi cancel during connect](/quest/m1/test-flakes-2/ffi-cancel-connect.md) - `client_cancel_aborts_a_pending_connect` orders its cancel deterministically, fixing the code if the race is real
- [Flapping-peer reorder seed](/quest/m1/test-flakes-2/flapping-peer-reorder.md) - replay one drill seed on current main, and fix it or delete the quest

## Related

- [Controlled time](/quest/m1/time/README.md) - the systemic fix these rules ask for: every crate on a controlled clock
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - tracks the media fixture's cold-start audio-tone flake, a player stall it shares with the mono pin
