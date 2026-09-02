# [XL] Drive raw QUIC from the selected core in moq-uring

## Goal

`moq-uring` drives the selected Quinn-family sans-IO core for raw moq-lite,
while preserving its thread-per-core workers, shared UDP socket strategy,
connection-ID steering, buffer ownership, pacing, GSO/GRO, and observable
shutdown behavior. The quiche adapter remains available until WebTransport
parity lands.

## Plan

Build one adapter around the selected core's input, timeout, event, and
transmit polling APIs. Extend the existing worker, socket, buffer-ring, and
connection tables rather than adding a second runtime. Keep packet storage
alive through every transmit completion and map pacing deadlines onto the
current uring executor without inventing a second pacer.

Reach raw-QUIC behavioral parity first: version negotiation, datagrams,
stream/reset lifecycle including `RESET_STREAM_AT`, idle and keep-alive timers,
connection close, certificate authentication, key logging, qlog, QUIC-LB IDs,
dual-stack binding, path MTU discovery, and session statistics. The
hierarchical scheduler must run inside the protocol core, not above it in
`moq-net`.

Run the existing raw-QUIC integration suite and `just test smoke-full`. Add
failure-path coverage for GSO fallback, timer expiry, worker shutdown, and
connection-ID routing. Benchmark chat, media 1:1, media fanout, handshake
churn, and idle-connection memory against the current quiche adapter on the
same worker and crypto configuration. Adoption requires no material latency,
capacity, or RSS regression and a written explanation of any CPU tradeoff.

Do not delete the quiche adapter in this quest. A native-only win does not
prove that browser WebTransport traffic can move.

## Required

- [Release the custom QUIC stack](/quest/m2/quic/release.md) - moq-uring must
  integrate immutable package versions
