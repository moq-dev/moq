# [M] moq-quic re-sends a lost ServerHello before its PTO

## Goal

Under heavy loss or corruption, a moq-quic handshake whose ServerHello was
lost recovers within a round trip of the evidence, instead of waiting out
PTOs backed off past 16 s. The QUIC Interop Runner's `handshakecorruption`
case passes for the moq-quic self pair, and its `known` entry in
`test/quic-interop/cases.toml` is gone.

## Plan

Found by [the handshake loss quest](https://github.com/moq-dev/moq/pull/8824),
which fixed the handshake timeout and the server's CID switch. The self
pair still failed `handshakecorruption` in one of nine runs afterwards. In the
failing connection's pcap, the server's Handshake flight reached the client
before the ServerHello did, so the client discarded it; its own probes were
PINGs (its ClientHello was already acknowledged), which the server answered
with bare ACKs; and once the ServerHello arrived, the client's anti-deadlock
probe and the server's next Handshake retransmission were both 8 to 16 s out.
The server heard nothing for 30 s and gave up.

RFC 9002 section 6.2.3 allows sending unacknowledged CRYPTO data before the PTO,
a limited number of times per connection:

- A client that receives Handshake packets before it has Handshake keys
  probes in Initial at once.
- A server that receives an ack-eliciting client Initial while its own
  Initial CRYPTO is unacknowledged re-sends that CRYPTO at once, within
  the anti-amplification limit.

Cover it on the seeded sim (`rs/moq-quic/src/tests/sim.rs`,
`handshake_under_loss`) with corruption, and rerun
`just test quic-interop --peers moq-quic --cases handshakeloss,handshakecorruption --roles server`
until it is stable.

Public API: none. Wire: none.

## Related

- [Public interop listing](/quest/m4/quic-interop-listing.md) - this cell is public there
