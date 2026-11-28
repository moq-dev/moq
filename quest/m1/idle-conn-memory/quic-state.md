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
in `Connection::spaces` for the connection's whole life. Moving them behind a
box that is freed once both are discarded means auditing the ~90
`spaces[...]` index sites that can name a discarded space: any site the audit
misses becomes a panic a peer can trigger. One way to do it: a `Spaces` type
whose reads of a discarded space return a shared empty space and whose writes
cannot reach one by construction.

The rest of the profile's items stay: the current and next 1-RTT keys
(precomputed so a spoofed key phase costs nothing), BBRv3's state, the stream
waits table (entries live only while a waiter is), and `moq_congestion::Lost`
(only a temporary `Vec` in `detect_lost_packets`; nothing keeps one).

## Waiting

- Maintainer: Should the Initial and Handshake packet spaces (1.6 KB inline per connection) move behind a box freed after the handshake, given that it means auditing ~90 index sites where a missed one is a remote-triggerable panic?
  - Separate quest after the 0.22.0 cut (Recommended) - an L-sized audit of the QUIC state machine on its own, with the fuzz target run over it
  - Do it in #9827 - finishes the Goal in one PR, but holds the measured 11.6 KB/conn heap saving behind a riskier refactor
  - Drop it - 1.6 KB is about 2% of the relay's 93 KB per idle connection
