# [M] Route every lite-07 SUBSCRIBE through TrackRun and check expiry while restaging

## Goal

Every lite-07 `SUBSCRIBE` (with or without an immediate subscriber `FIN`) runs through `TrackRun` in both Rust (`rs/moq-net`) and JS (`js/net`), with no separate `OneShotFetch` / `#serveOneShotFetch` engine, so wire semantics never race on `FIN` arrival, Rust and JS agree on `Largest`, and `Range` policy (`max_age`, `min_age`, and per-range priority/order) applies uniformly. `Recover::poll` also checks expiry before parking while a track restages.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit):
- ✅ Delete `OneShotFetch` and route every lite-07 `SUBSCRIBE` through `TrackRun` (rejected: keeping immediate `NOT_FOUND` on unproduced one-shot fetches via a separate explicit wire rule).

Caused by [moq-dev/moq#9687](https://github.com/moq-dev/moq/pull/9687) (which kept a dual engine despite an Opus high finding and accepted a `Recover::poll` finding under `afk`).

1. **Delete `OneShotFetch` / `#serveOneShotFetch`:**
   - In Rust (`rs/moq-net/src/lite/publisher.rs:2526-2575, 2695-2702, 3250-3554`), delete `OneShotFetch`, `PendingOneShot`, `OneShotRequest`, and `single_group_position`; route every lite-07 `SUBSCRIBE` through `TrackRun`, which already handles `peer_fin` and bounded completion (`publisher.rs:6806-6816`).
   - In JS (`js/net/src/lite/publisher.ts:1593-1599, 1630-1782`), delete `#serveOneShotFetch` (which also diverged by hardcoding `largest: "unknown"` while Rust sent `track.live_edge()`).
   - Update any tests expecting immediate `NOT_FOUND` for an unproduced one-shot group on a track with no fill handler to match normal bounded `SUBSCRIBE` (waiting until `SUBSCRIBE_END` or explicit `SKIP`/`NOT_FOUND` from a fill handler), and verify `drafts/draft-lcurley-moq-lite.md:236` matches.
2. **Check expiry while restaging in `Recover::poll`:**
   - In `rs/moq-net/src/model/resume.rs:4309-4310`, check `self.copy.poll_stale(...)` (using `self.poll_budget(waiter)`) before returning `Poll::Pending` on `(None, Cause::Cut)` so a live reader does not ignore superseding groups on `self.copy` while a parked track restages.

Tests: Rust and JS unit tests verifying `SUBSCRIBE` + same-packet `FIN` and `SUBSCRIBE` + delayed `FIN` produce identical wire responses (`Largest`, `SUBSCRIBE_START`, `Range` policy, and delivery/end), plus a `resume.rs` regression test where `Recover::poll` settles stale during `(None, Cause::Cut)` restaging. Run `just test interop --all`.

Public API: none. Wire: eliminates a lite-07-wip `FIN` timing race and Rust/JS `Largest` divergence before `lite-07` freezes.

## Related

- [Per-range priority, order, and judged OLD drops on lite-07](/quest/m2/lite07-subscribe/per-range-policy.md) - shares `TrackRun` range policy handling
