# [S] More tests hold up under load

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
- A mocked clock still flakes when the code jitters its timers with `rand`
  (the draft-16 cancel race was a retry sweep jittered past a fixed settle
  budget). Wait out the jitter's whole window, not a count of small steps.

This README's own work, after the children: run `just check --all` several
times on a loaded machine, as the first round did.

The [redundant failover drift](/quest/m0/redundant-failover-drift.md) moved
to m0 on 2026-10-20 (maintainer), since it may hide a TS export bug and fails
loaded `just check` runs.

Added 2026-10-20 (maintainer): three more from that day's merge batch, the
draft-16 cancel race first since it fails on a mocked clock and so may hide
an ordering bug. The failed-listen test was meant to join the redial quest,
which #6032 had already finished, so it reuses #6032's reserved-port fix as
its own child. moq-uring's `host_tracks_moq_clock` was seen the same day
but #6056 had already fixed it by measuring the calibration gap.

Added 2026-10-21 (maintainer): two flakes (OBS drain, js/net RSS) from the
m0/m1 merge run, kept in one child since each is small.

Public API: none. Wire: none.

## Related

- [Controlled time](/quest/m1/time/README.md) - the systemic fix these rules ask for: every crate on a controlled clock
- [Publish channel count](/quest/m1/publish-audio-channel-count.md) - tracks the media fixture's cold-start audio-tone flake, a player stall it shares with the mono pin
