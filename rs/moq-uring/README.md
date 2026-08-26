# moq-uring

Experimental Linux io\_uring support for the native MoQ stack: a
thread-per-core `Worker` that owns a `SINGLE_ISSUER | DEFER_TASKRUN |
COOP_TASKRUN` ring, a userspace timer heap, a local (`!Send`) task set, and
the UDP sockets bound through it.

- **Receive**: one persistent multishot `recvmsg` per socket, fed from a
  registered provided-buffer ring of worst-case-sized buffers (one per
  completion), with `UDP_GRO` coalescing. Received packets borrow the pool and
  hand the space back on drop, which is also the receive-side backpressure.
  Incremental consumption (`IOU_PBUF_RING_INC`) cannot back a multishot
  `recvmsg`: the kernel faults the receive once a buffer's leftover tail is
  smaller than the recvmsg header.
- **Send**: `sendmsg` with an explicit `UDP_SEGMENT` control message per call,
  staged in a fixed pool of buffers owned by id and released on completion
  (the shape a later `SENDMSG_ZC` needs).
- **Timers**: a heap the worker sweeps; the earliest deadline rides
  `io_uring_enter` as an absolute timeout. Zero timeout SQEs. The worker's
  `Handle` implements `moq_net::Timers`.
- **Parking**: a futex word per worker. Remote wakes are an atomic store, plus
  one `futex(2)` wake only while the worker is actually parked (a `FUTEX_WAIT`
  SQE armed on the word).
- **QUIC**: sans-IO quiche over that UDP path. A `quic::Endpoint` serves many
  connections on one socket, demuxed by connection id (dials share the socket
  with accepts, ids rotate as peers consume them, unknown versions get a
  version negotiation packet). Native peers speak raw QUIC: the ALPN carries
  the application protocol.
- **WebTransport**: browsers negotiate `h3` and `quic::web::Request` runs the
  HTTP/3 CONNECT handshake (SETTINGS, subprotocol selection, capsule close)
  over the same adapter via `web-transport-proto`. `quic::web::Session` is
  the one transport type the runtime drives, raw or web (`Session::raw`), so
  `connect_lite`/`accept_lite` run moq-lite sessions on the worker either
  way, with stream and close codes mapped through the HTTP/3 error space in
  web mode.
- **Steering**: an endpoint whose socket sits in a `moq-sock` steered
  `SO_REUSEPORT` group sets `endpoint::Config::shard`, and every issued
  connection id leads with the group's steering byte, so the kernel keeps a
  connection (and a cluster dial's responses) on the worker that owns it.

Requires **Linux 6.12**; `Worker::new` refuses older kernels with a legible
error rather than degrading (note that default container seccomp policies
block io\_uring entirely). There is no fallback here: older kernels keep using
the tokio stack.

## Validation

`tests/echo.rs` runs a raw [quiche](https://github.com/cloudflare/quiche) echo
over the worker: handshake, half a megabyte each way, timers driven by
quiche's own timeout. `tests/session.rs` runs full moq-lite sessions through
`quic::Endpoint` (including two clients demuxed on one server socket), and
`tests/endpoint.rs` covers the endpoint mechanics (dial+accept on one socket,
version negotiation, the dial-only refusal), `tests/workers.rs` runs a
steered two-worker reuseport group serving one port across threads, and
`tests/web.rs` is WebTransport interop against `web-transport-quinn` (the
stack browsers interop with): stream/datagram echo through the H3 framing,
close codes through the capsule, and a full moq-lite session over
WebTransport. All of them skip (loudly) below the kernel floor, which
includes GitHub-hosted CI runners.

## Benchmarks

`udp_tokio` and `udp_uring` are the disposable syscall-level matrices from the
first spike (recv batching x GRO x GSO, epoll vs io\_uring); see git history
for their methodology. `echo_quiche` is the ablation matrix over the real
worker: the same quiche echo with receive batching, GRO, and GSO toggled one
at a time.

```bash
just rs bench-udp --sample-size 20 --measurement-time 2 --warm-up-time 1
just rs bench-echo
```
