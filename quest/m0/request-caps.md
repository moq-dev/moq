# [L] Peer-declared lengths and request counts are bounded

## Goal

Every length and count a peer declares is bounded before the relay buffers
or tracks it. That covers three things:

- lite control messages, which may declare 64 MiB each
  (`lite/message.rs`, buffered by `coding/reader.rs` `poll_read_more`);
- IETF FETCH object properties, decoded as a bare `Vec<u8>` with no cap;
- IETF incoming request IDs, which are never checked against the
  `MAX_REQUEST_ID` we advertise on drafts 14 to 16 (`u32::MAX`,
  `rs/moq-net/src/server.rs` and `client.rs`).

Announces and subscriptions per session are capped as well.

## Plan

- Lite: per-message `MAX_SIZE` sized to the largest legitimate message
  (SETUP already caps at 64 KiB). Anything larger is a decode error.
- Audit `rs/moq-net/src/coding/` for any other decode that allocates from a
  peer length, and cap the fetch properties. This absorbs the deleted
  `coding-reader-cap` quest.
- IETF drafts 14 to 16: advertise a finite `MAX_REQUEST_ID` window, grant more with
  MAX_REQUEST_ID as requests close, and close with the draft's error on an ID
  past it (`ietf/control.rs`, `ietf/adapter.rs`). Draft-17 removed
  MAX_REQUEST_ID; there QUIC's bidi stream limit and the per-session cap
  bound requests.
- A per-session cap on live announces and subscriptions on both protocols,
  refused per request rather than session-fatal where the protocol allows.
- Benchmark per-session request churn swept over sessions and requests per
  session, so the window refill shows no slope.
- Mirror the lite message cap in `js/net`.

Public API: possibly caps on the session config; propose the shape in the PR.
Wire: behaviour within the drafts' existing limits. No format change.

## Required

- [IETF FIN semantics](/quest/m0/ietf-fin-not-cancel.md) - MAX_REQUEST_ID refills as requests close, and that change decides when a request closes
