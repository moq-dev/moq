# [M] WebRTC sessions report their wire bytes

## Goal

A `moq-webrtc` session (WHIP ingest and WHEP egress) exposes a
`moq_net::transport::Wire` whose sent and received byte counters include
everything the session puts on the network: RTP and RTCP with their SRTP
overhead, DTLS, and STUN/ICE. A relay registers it with `moq_net::stats::Session::wire`
like a native session's, so a WebRTC session's wire bytes land in its tier's
stats instead of only its payload bytes.

## Plan

[moq-dev/moq#6454](https://github.com/moq-dev/moq/pull/6454) removed an
unused wire source from `moq-webrtc`; this brings one back with a consumer: a
downstream deployment meters delivery by wire bytes, and until this lands it
keeps WebRTC on payload bytes, which undercounts by the packet and crypto
overhead (about 4% for video).

- Count at the socket the session owns (the ICE agent's UDP send and receive
  path), so retransmissions and every protocol layer count once, rather than
  summing per-protocol estimates.
- A shared or muxed socket (one port serving many sessions, see
  [one port](/quest/m3/one-port/README.md)) attributes each datagram to the
  session its demux routes it to; unattributed datagrams (a STUN probe for no
  session) count nowhere.
- Test with a loopback WHIP publish and WHEP play: each side's wire bytes are
  at least its payload bytes plus the SRTP overhead per packet, and the
  sender's sent bytes match the receiver's received bytes on a lossless link.
