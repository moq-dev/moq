# [M] Rust request and follow

## Goal

Rust names the two resolutions `request(path, epoch)` and `follow(path,
epoch)` (today's `announced_broadcast`). The immediate table lookup stays
private, `routed` and `routed_broadcast` are deleted with their callers
fixed, and the route-level `Follow` is private.

## Plan

Split from [Two broadcast resolutions](/quest/m1/api-audit/resolve/README.md)
on 2026-10-28 (maintainer); its Plan holds the behavior already landed in
[moq-dev/moq#6445](https://github.com/moq-dev/moq/pull/6445).

- Preserve terminal selected-handler refusals, private deferred validity
  checks, and instance identity across route churn.
- Move the remaining route-level `Follow` consumers before making it private:
  the moq-hls server's resolve, with its hand-rolled discovery and
  `RESOLVE_TIMEOUT`, and the tests that read its events. `moq play` and
  `moqsrc` already follow through `moq_mux::Source::follow`. Add no new public
  route API or compatibility shim to make the privatization pass.
- Fix the in-tree Rust callers of `routed` and `routed_broadcast` that this
  deletion breaks (the CLI, gateway, mux binding, relay HTTP), keeping their
  route-change retry where they need it. Reworking their resolution logic
  beyond the rename belongs to [Callers resolve once](/quest/m1/api-audit/resolve/callers.md).

Tests: an announced but unwatched path over lite-05, 06 and 07-wip; synced
absence; discovery retention and release; pinned follow return; unchanged
explicit refusal.

Public API: breaking in moq-net, with an Impact migration note. Wire: none.
