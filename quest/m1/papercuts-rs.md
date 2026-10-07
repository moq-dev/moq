# [S] Rust papercuts from the 2026-10-07 audit

## Goal

Four small Rust fixes from the 2026-10-07 code audit, each with a regression
test that fails without it:

- HTTP `/announced` and `/fetch` refusals count in the relay's refusal
  metrics, as QUIC, WebSocket, and io_uring refusals already do.
- A replaced route whose newest group is `u64::MAX` keeps delivering that
  group from the old copy instead of being capped at group 0.
- A future draft version sends stream-reset `GOING_AWAY` as 0x4, not as an
  internal error.
- A `UDP_GRO` cmsg of 0 cannot panic an io_uring worker.

## Plan

- `rs/moq-relay/src/web.rs`: both `admit_http` call sites (`serve_announced`
  and `serve_fetch`) return the error with `?` and never call
  `cluster.refusals.record`. The missing-subscriber 401 in `serve_announced`
  is uncounted too. Record each refusal the way `websocket.rs` does.
- `rs/moq-net/src/model/resume.rs` (around line 267): `Position::after_group`
  returns `None` for the last group, meaning "no end", and
  `unwrap_or_default()` turns that into position 0. Treat `None` as unbounded,
  as `let_go` in the same file already does. lite-07 varints can carry
  `u64::MAX`, so this is reachable from the wire.
- `rs/moq-net/src/ietf/error.rs`: `has_going_away` lists Draft18 through
  Draft22 explicitly. Flip it to `!matches!(older drafts)` like the
  request-error helper next to it, so a new draft variant falls forward.
- `rs/moq-uring/src/udp.rs`: `RecvMeta::parse` stores a stride of 0 as
  `Some(0)`, and `Packet::segments` then calls `chunks_mut(0)`. Drop a zero
  stride in the parser. Linux only emits `UDP_GRO` when `gso_size != 0`, so
  this is defensive and lives in m1, not m0's peer-input hardening (decided
  2026-10-07).

Public API: none. Wire: none (the `GOING_AWAY` change only affects drafts
that do not exist yet).

## Related

- [JS papercuts](/quest/m1/papercuts-js.md) - the JavaScript half of the same audit
