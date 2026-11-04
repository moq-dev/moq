# [M] A higher-priority group makes room

## Goal

When an open waits for stream credit, the session makes room for it from a
lower-priority stream: it hands that stream over to the new group at once if
all the stream's data is acknowledged, and otherwise resets the
lowest-priority stream, with at most one reset in flight per waiting open, so
a slow MAX_STREAMS never cuts more streams than needed. A group that lost its
stream resumes from where it stopped when it is next served.

## Plan

See the [epic](/quest/m1/stream-credit/README.md) for the decisions.
Decided 2026-11-04 (maintainer):

- **Takeover first.** A takeover needs no credit, so the new group starts at
  once. Only a stream whose sent data is all acknowledged qualifies, since a
  QUIC stream is ordered and the new group's bytes would otherwise queue
  behind the old group's retransmissions.
- **Cancel only as the fallback**, and the lowest-priority stream, not just
  idle ones. One reset in flight per waiting open, because credit returns
  only when the peer's MAX_STREAMS arrives, at least a round trip later.
- **When.** Rust acts as soon as its QUIC stack reports it is blocked. JS
  acts once an open has waited about one round trip (from WebTransport's
  `getStats()` RTT), not a fixed time: a fixed 100 ms is too long on a LAN
  and too short on mobile.
- **Wire.** Takeover needs lite-07 to let a group stream end one group and
  start another. Add it to `drafts/draft-lcurley-moq-lite.md` before lite-07
  finalizes; moq-transport and older lite versions only cancel.

Tests: a browser over WebTransport and a Rust relay, more live groups than
the cap, the higher-priority group starting without waiting a round trip;
a cap with no acknowledged stream resets exactly one stream per waiting open;
a group that lost its stream resumes without loss.

Public API: none. Wire: lite-07 group stream takeover.

## Required

- [One priority entry point for streams](/quest/m1/stream-credit/scheduler.md) - the entry point that decides when room is needed
