# [S] Lite datagrams obey one size limit on every hop

## Goal

A lite datagram that one hop accepts is never silently dropped by the next
for its size. Rust and `@moq/net` send and accept datagram bodies under the
same limit the draft states.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred,
inherited.

The draft says a publisher MUST NOT send a datagram body over 1200 bytes and
a receiver MUST drop one. The code sends anything up to
`session.max_datagram_size()` (`rs/moq-net/src/lite/publisher.rs:3619`, JS
`js/net/src/lite/publisher.ts:1632`) and accepts larger bodies on receive
(near `lite/subscriber.rs:1391`, JS `datagram_stream.ts`). Over a hop with a
larger QUIC limit we send and accept a 1300-byte body that a relay then
cannot forward onto a 1200-byte hop, so it vanishes mid-path.
[E2EE](/quest/m2/e2ee/README.md) sizes its plaintext on the 1200 cap.

Decided 2026-10-25 (maintainer): enforce 1200 end to end. Cap sends at the
smaller of 1200 and the transport limit, and drop larger bodies on receive,
in both languages. A per-hop limit was rejected: it loses datagrams silently
mid-path.

Public API: none. Wire: none (matches the draft as written).
