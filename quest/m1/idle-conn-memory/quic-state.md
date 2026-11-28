# [M] An established idle QUIC connection drops what it no longer needs

## Goal

Once the handshake is confirmed, a moq-quic server connection no longer
holds the buffers and state only the handshake uses, while what later code
reads stays (the negotiated parameters, and the peer identity WebTransport
reads through `TlsSession::peer_identity` after CONNECT). An idle
connection's sent-packet tracking holds about what its few in-flight packets
need rather than a 64-slot chunk.

## Plan

[moq-dev/moq#9827](https://github.com/moq-dev/moq/pull/9827) landed most of it:
a confirmed server frees its rustls session, sent-packet chunks start at 4
slots, discarded packet spaces free their buffers, the scheduler frees its
emptied B-tree roots, and `prev_path` is boxed. In the moq-quic sim, an idle
server connection's heap went from 17.8 KB to 6.2 KB and its inline
`Connection` from 8,240 B to 7,712 B.

What remains is the Initial and Handshake `PacketSpace`s, 800 B each, inline
in `Connection::spaces` for the connection's whole life. Move them behind a
box that is freed once both are discarded. That means auditing the ~90
`spaces[...]` index sites that can name a discarded space, since any site the
audit misses becomes a panic a peer can trigger. One way to do it: a `Spaces`
type whose reads of a discarded space return a shared empty space and whose
writes cannot reach one by construction. The audit runs the `fuzz` connection
target over the state machine before the PR is ready, so a missed site shows
as a crash there rather than in production.

The maintainer chose (2026-11-28) to do this as its own work after the 0.22.0
cut, not in #9827 and not dropped.

The rest of the profile's items stay: the current and next 1-RTT keys
(precomputed so a spoofed key phase costs nothing), BBRv3's state, the stream
waits table (entries live only while a waiter is), and `moq_congestion::Lost`
(only a temporary `Vec` in `detect_lost_packets`; nothing keeps one).

## Parked

- until the 0.22.0 cut ships - the maintainer deferred it past the cut; move it to m3 (`quest mv`) once #9827 merges and releases the claim, then delete this section
