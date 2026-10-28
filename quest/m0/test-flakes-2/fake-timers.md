# [M] JS tests drive time through Manual

## Goal

The remaining `@moq/net` and `@moq/hang` tests that use bun's fake timers run
on the `Manual` clock instead, so host-task yields cannot race them: forward,
the ietf and lite publisher and subscriber tests, integration, connect, and
hang's consumer startup.

## Plan

Follows [moq-dev/moq#6988](https://github.com/moq-dev/moq/pull/6988), which
made `Budget` yield through the clock and moved three tests. Bun's fake
timers cannot see a yield, so these pass today only because nothing yields
at the wrong moment. Prove each converted test still fails when its behavior
breaks, and that it passes with `Budget` forced to yield on every call.

Public API: none.
