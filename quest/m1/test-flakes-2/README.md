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

Public API: none. Wire: none.

## Required

- [Interop close code](/quest/m1/test-flakes-2/interop-close-code.md) - the browser reliably sees `unauthorized` for a refused session
- [Auth refresh mocks time](/quest/m1/test-flakes-2/auth-refresh-mock-time.md) - moq-relay's auth refresh and fixed-lease tests measure a grant's life on one mocked clock
- [uring teardown close](/quest/m1/test-flakes-2/uring-teardown-close.md) - moq-uring's published-close teardown test holds under load, fixed at its cause

## Related

- [Controlled time](/quest/m1/time/README.md) - the systemic fix these rules ask for: every crate on a controlled clock
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - tracks the media fixture's cold-start audio-tone flake, a player stall it shares with the mono pin
