# [S] Honor router blocked-tier verdicts when a lite-07 subscriber omits node

## Goal

When a lite-07 subscriber pins an `Identity` without naming a `node`, `poll_node_filter` honors the router's verdict instead of discarding it, so a superseded epoch is never served while a higher epoch on an unavailable tier blocks it, verified by a real multi-relay cluster test with scoped grants.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ fix in `m3` (rejected: leaving unnamed-node lookups discarding the router verdict).

Caused by [moq-dev/moq#9610](https://github.com/moq-dev/moq/pull/9610) (where this finding was refuted as pre-existing under `afk`, and no real-cluster scoped-grant test was run).

- In `rs/moq-net/src/lite/publisher.rs:197-209`, `poll_node_filter` executes `let _ = ready!(origin.route_node().poll_forward(..., None, &claim, waiter));`, discarding the router's verdict. When epoch 2 at `src` is unavailable (blocking epoch 1 at `src/a` in `Router::tier`), `origin.lookup()` can still serve superseded epoch 1, violating lite-07's Resolution rule.
- Inspect the returned `Forward` / router verdict in `poll_node_filter` and refuse or wait when the router blocks the superseded tier, matching named-node lookups.
- Add an integration test exercising a real lite-07 relay cluster with scoped grants where epoch 2 is unavailable and an unnamed-node subscriber pins the claim.

Public API: none. Wire: none.
