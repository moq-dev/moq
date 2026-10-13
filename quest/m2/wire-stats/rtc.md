# [S] moq-rtc wire source

## Goal

moq-rtc gives its embedder each WebRTC peer's cumulative wire bytes, both
directions, for WHIP and WHEP: every STUN, DTLS, SRTP, and SRTCP datagram's
UDP payload. These counts exclude the IP and UDP headers, the same
definition as QUIC's `udp_tx`.

## Plan

- Every outbound datagram leaves through one site, `Output::Transmit` →
  `socket.send_to` (`rs/moq-rtc/src/session.rs`). Sum `contents.len()`
  there. Count inbound datagrams where the mux hands them to the peer. This
  needs no str0m stats interval.
- Hand the embedder a cumulative `(tx, rx)` source per peer, in the shape
  [Presence wire counters](/quest/m2/wire-stats/presence.md) registers.
- Test: a WHEP session's reported tx covers at least the RTP payload it
  sent.
