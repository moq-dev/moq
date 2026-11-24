# [M] WebRTC sessions report their wire bytes

## Goal

A `moq-webrtc` session (WHIP ingest and WHEP egress) registers a wire source
that `moq-net`'s stats reads at report time, counting everything the session
puts on the network: RTP and RTCP with their SRTP overhead, DTLS, and
STUN/ICE. Its wire bytes then land in its tier's stats beside native
sessions', instead of only its payload bytes.

## Plan

Decided 2026-11-23 (maintainer): ✅ one pull-on-report wire source for every
transport (rejected: a second push-counter `transport::Wire` in
`moq-webrtc`). Stats reads each registered session's source when a report
fires, through one source trait that QUIC's counters, a TCP connection's
`TCP_INFO` (for RTMP gateways), and this session's socket all implement. No
timer: the report pulls a fresh reading, and the final reading is taken on
close. The trait is being built upstream for the `TCP_INFO` consumer first;
if it has not landed when this starts, this quest adds it.

[moq-dev/moq#6454](https://github.com/moq-dev/moq/pull/6454) removed an unused
wire source from `moq-webrtc`; this returns with a consumer: a downstream
deployment meters delivery by wire bytes and until then keeps WebRTC on its
payload meter, which undercounts by the packet and crypto overhead (about 4%
for video).

- Count at the socket the session owns (the ICE agent's UDP send and receive
  path, `ice/agent.rs` and `ice/wire.rs`), so retransmissions and every
  protocol layer count once, rather than summing per-protocol estimates.
- A shared or muxed socket (see [one port](/quest/m3/one-port/README.md))
  attributes each datagram to the session its demux routes it to; a datagram
  for no session (a stray STUN probe) counts nowhere. The demux sends a STUN
  answer to both an address's holder and its claimant, so count received
  bytes in the ICE agent rather than at the demux, and count a STUN answer
  only in the agent whose sent checks hold its transaction ID (the list
  `answers_check` in `ice/agent.rs` already keeps).
- Test with a loopback WHIP publish and WHEP play: each side's wire bytes are
  at least its payload plus the SRTP overhead per packet, and the sender's
  sent bytes match the receiver's received bytes on a lossless link.
