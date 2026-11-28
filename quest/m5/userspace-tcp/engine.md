# [XL] A sans-IO TCP connection in moq-quic

## Goal

`moq-quic` has a sans-IO TCP connection that accepts connections from any
standard TCP peer, such as Linux, macOS, Windows, and the browsers' stacks, and
carries data both ways. Its send side follows a `moq_congestion::Controller` and
the shared pacer, and pulls new bytes from its source only when a segment can
leave. Bytes once sent are resent only by retransmission, which they stay
committed to until acknowledged.

## Plan

See [the epic](/quest/m5/userspace-tcp/README.md) for why it is our own stack.
Recommendations, by the agent:

- Shape it like the QUIC and QMux connections: feed received segments in, write out
  what `poll_transmit` returns, and run timers at `poll_timeout`. The source is a
  pull interface (give me up to N bytes now), so QMux can serialize frames at
  that moment; a plain byte buffer source serves the `AsyncRead`/`AsyncWrite`
  stream.
- Server side of the handshake only, as the epic settles. SYN cookies from the
  start: a half-open connection is state a hostile peer makes us hold, so none is
  kept until the ACK proves the address. Bound every per-connection buffer by
  the advertised window, not by a constant.
- Loss recovery: SACK, timestamps, window scaling, and RACK-TLP, reusing
  `moq-quic`'s loss-detection and RTT code where the shape fits. A
  retransmission resends the exact committed range, whatever the source now
  holds.
- Receive side: in-order reassembly with SACK generation, a receive window that
  applies the consumer's backpressure, and delayed ACKs in line with RFC 9293.
- Test on `moq-quic`'s sim with mocked time, and differentially against Linux
  kernel TCP. Over a veth in a netns, a kernel peer and this stack each send the
  same byte stream through the same bufferbloated queue; both must arrive
  intact, and the RTT and delivery-rate estimates must agree within what each
  stack's sampling allows. Also fuzz the segment parser.

## Related

- [AF_XDP backend](/quest/m5/userspace-tcp/xdp.md) - the packet I/O this runs on in production
