# [L] A model of a relay chain's failover delivers every group once

## Goal

A TLA+ model of one track served to a client through a chain of relays
(client, relay, relay, origin, with an alternate path) checks that when a
relay dies or a route moves at any hop, the client gets every group its
budget allows, each once and in order, and the track never ends while the
broadcast is live.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan). Resume failover has caused many
production issues, and each subsystem model in [moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)
checks one layer alone: routing picks the next hop (`verify-route`), a front
switches copies (`verify-resume`), and a subscription accounts for each group
(`verify-subscribe`). This composes them at the level where they interact:
each relay's front switching routes while the relay below it is itself
resuming.

- Abstract each layer to the steps its own model proved, citing that model,
  so the state space stays small: a route change is one step, a copy switch is
  the resume model's switch, a group's delivery is the subscribe model's
  accounting.
- Rules: each group within the client's budget reaches it exactly once, in
  order; no group is skipped while an alternate path holds it; the track
  ends only after the origin ends it; no relay keeps a copy after the client
  leaves.
- Replay each counterexample on the real stack (relays over the memory
  transport, mocked clock), and pin confirmed bugs as `residual_*` tests.

Related: [route health](/quest/m5/route-health/README.md), which adds
stall-driven failover; extend this model when it lands.

Public API: none. Wire: none.
