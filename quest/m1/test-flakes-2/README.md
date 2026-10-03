# More tests hold up under load

## Goal

A second round after [#4286](https://github.com/moq-dev/moq/pull/4286):
tests that pass alone but have failed under a loaded `just check` pass
reliably, each fixed at its cause, never by raising a timeout or adding a
retry.

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

This README's own work, after the children: run `just check --all` several
times on a loaded machine, as the first round did.

Public API: none. Wire: none.

## Required

- [moq-cli tests on a paused clock](/quest/m1/test-flakes-2/cli-paused-clock.md) - the fetch timeout and completion tests stop racing wall-clock budgets
- [Subscription cut by disconnect](/quest/m1/test-flakes-2/subscription-cut.md) - a publisher disconnect never ends a subscription clean
- [Media late join](/quest/m1/test-flakes-2/media-late-join.md) - `just test media` late join stays within one GOP, or the regression is fixed
- [js/publish audio clock](/quest/m1/test-flakes-2/publish-audio-clock.md) - the audio encoder delay test runs on mock time

## Related

- [Archive enrollment](/quest/m1/archive/enrollment-flake.md) - the same
  kind of flake on the archive line, where its test lives
