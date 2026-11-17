# [M] A WHEP viewer ends with its broadcast and costs one line per kind

## Goal

A WHEP session ends, closing its peer connection, once its broadcast's
catalog ends cleanly and the pumps have written what they hold, so a viewer
learns to reconnect instead of freezing forever. An offer is answered with at
most one audio and one video media line, so one viewer cannot multiply the
server's per-viewer work. A client re-offering from the same address reaches
its new session.

## Plan

Found by the 2026-11-17 pre-cut correctness audit, from source; reproduce
first. All are on `release` too.

- `rs/moq-webrtc/src/session.rs` `run_loop`: a clean catalog end clears
  `catalog_end` and keeps looping on `writes_rx`, whose sender
  `EgressSource` holds for the session's life, so the session never ends.
  [moq-dev/moq#6649](https://github.com/moq-dev/moq/pull/6649) said
  `Response::run` fails once the publisher leaves; that holds only for an
  unclean leave. Recommended: each pump owns its sender, and the session ends
  `Ok` and closes once the writes channel drains after the catalog ends.
  Repro: RTMP `deleteStream` while a WHEP viewer plays.
- `session.rs`, `egress.rs`, `sdp.rs`: an offer with K video lines gets K
  pumps, each with its own subscription, packetization, encryption, and
  3000-packet buffer. Recommended: reject extra lines as data channels
  already are. [RTC pacing](/quest/m3/rtc-pacing.md) caps bandwidth only.
- `server/mux.rs`: a cached source address outranks the STUN username, so a
  re-offer from the same address goes to the old session. Route binding
  requests by username.

Public API: none. Wire: none.

## Related

- [RTC pacing](/quest/m3/rtc-pacing.md) - paces WHEP viewers to a cap
