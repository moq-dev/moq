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

- [Media late join](/quest/m1/test-flakes-2/media-late-join.md) - `just test media` late join stays within one GOP, or the regression is fixed
- [Import catalog finish](/quest/m1/test-flakes-2/import-catalog-finish.md) - `moq-cli`'s subprocess EOF catalog-finish test holds up under load with event-based fixture coordination
- [Relay restart rebind](/quest/m1/test-flakes-2/relay-restart-rebind.md) - the crash drill restarts on its original UDP address under concurrent load
