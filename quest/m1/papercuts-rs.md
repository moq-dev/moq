# [S] Rust papercuts from the 2026-10-07 audit

## Goal

Two small Rust fixes from the 2026-10-07 code audit, each with a regression
test that fails without it:

- HTTP `/announced` and `/fetch` refusals count in the relay's refusal
  metrics, as QUIC, WebSocket, and io_uring refusals already do.
- A replaced route whose newest group is `u64::MAX` keeps delivering that
  group from the old copy instead of being capped at group 0.

## Plan

- `rs/moq-relay/src/web.rs`: both `admit_http` call sites (`serve_announced`
  and `serve_fetch`) return the error with `?` and never call
  `cluster.refusals.record`. The missing-subscriber 401 in each handler is
  uncounted too. Record each refusal the way `websocket.rs` does.
- `Copy::wire` in `rs/moq-net/src/model/resume.rs`: `Position::after_group`
  returns `None` for the last group, meaning "no end", and
  `unwrap_or_default()` turns that into position 0. `until` is
  `Option<Option<u64>>`, and an inner `None` (replaced before any group) must
  still end at 0; only `after_group`'s overflow is unbounded. Reuse `let_go`'s
  `until.map_or(Some(0), |last| last.checked_add(1))` and test both cases.
  lite-07 varints can carry `u64::MAX`, so this is reachable from the wire.

Decided 2026-10-08: the `GOING_AWAY` fallback for future drafts and the
zero `UDP_GRO` stride were dropped. Neither is reachable today: no newer
draft exists, and Linux never emits a zero stride.

Public API: none. Wire: none.

## Related

- [JS papercuts](/quest/m1/papercuts-js.md) - the JavaScript half of the same audit
