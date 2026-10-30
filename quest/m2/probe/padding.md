# [M] Padding probes from the browser

## Goal

A browser session (`js/net` over WebTransport) probes up to its target with a
lowest-priority padding stream on an ALR schedule, and a shaped link in Chrome
proves it: with padding the estimate rises past the media, without it it stays
frozen, and on a path with no room padding stays within its overhead and media
frame-delay budgets. Such sessions on lite-07 or IETF draft-18 and later then
have a sender, so [viewer targets](/quest/m2/probe/viewer-target.md) can
advertise PROBE `Increase` for them.

## Plan

The sender landed in [moq-dev/moq#7390](https://github.com/moq-dev/moq/pull/7390):
`js/net/src/padding.ts` (the `Schedule` and the `Padding` writer),
`Allocator.setTarget` and `Reservation.probed` mirroring Rust's send target,
and `reload.ts` following the allocator's target into the live session's
sender. Its constants (cluster length, the GoogCC ALR thresholds, the overhead
credit of 10% of the estimate) are untuned: no browser on a real network has
run it yet.

What remains is the browser test, decided with the maintainer on 2026-10-17
(see the [epic](/quest/m2/probe/README.md)):

- In Chrome through a shaped link: with padding the estimate rises past the
  media, without it it stays frozen. On a path held below the target, report
  media frame delay and deadline misses against budgets set before tuning:
  lowest priority can't preempt padding already in flight or queued at the
  bottleneck, so a frame ready just after a cluster can still wait behind it.
  Assert that repeated clusters on a path with no room stay within the
  overhead budget.
- Wire it into CI at least nightly, sharing the epic's shaped-uplink setup
  (the nightly already plays the matrix in headless Chromium through
  moq-shaper).
- Check what Chrome's `estimatedSendRate` actually does under padding. A
  Cubic estimate (cwnd over RTT) grows slowly during a short cluster, and a
  BBR estimate aged down to the media keeps the session from reading as
  app-limited at all; either may call for retuning the schedule.
