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
  `Handle::run` drives MoQ with the worker clock and a single timer.
- **Parking**: a futex word per worker. Remote wakes are an atomic store, plus
  one `futex(2)` wake only while the worker is actually parked (a `FUTEX_WAIT`
  SQE armed on the word).
- **QUIC**: a sans-IO QUIC stack over that UDP path. A `quic::Endpoint` serves
  many connections on one socket, demuxed by connection id (dials share the
  socket with accepts, ids rotate as peers consume them, unknown versions get a
  version negotiation packet). Native peers speak raw QUIC: the ALPN carries
  the application protocol.
- **WebTransport**: browsers negotiate `h3` and `quic::web::Request` runs the
  HTTP/3 CONNECT handshake (SETTINGS, subprotocol selection, capsule close)
  over the same adapter via `web-transport-proto`. `quic::web::Session` is
  a raw or web transport (`Session::raw`). `connect_lite`/`accept_lite` return
  the session and its driver; poll the driver or await it inside a
  `Handle::spawn` task to run it on the worker. Web mode maps stream and close codes through the
  HTTP/3 error space.
- **qlog**: `quic::qlog::Sink` points a group of workers at a directory and
  `quic::Transport::qlog` turns capture on. The pinned worker never writes to
  the file: the QUIC stacks want a `Send + Sync` writer, which cannot hold the
  worker's `!Send` ring handle, so a trace is staged in memory and handed to
  one background thread for every worker sharing the sink. Behind the `qlog`
  feature, so a production build compiles none of it.
- **Metrics**: a set of relaxed atomic counters per worker, read from any
  thread through `metrics::Metrics::snapshot`. Buffer-pool health (`ENOBUFS`,
  provided-buffer exhaustion, TX-pool stalls), batch effectiveness (datagrams
  per receive and per send), ring traffic (submissions, completions,
  `io_uring_enter` calls), and scheduling (parks, remote futex wakes, timer
  churn). Pass a `metrics::Metrics` to `Config::metrics` to hold a copy on the
  thread that spawned the worker, or read the worker's own with
  `Handle::metrics`. `moq-relay` publishes them at `/metrics` on its internal
  listener.
- **Identity**: the socket names its worker. `Handle::udp` adopts a lone
  `UdpSocket` or a member of a completed `moq-sock` steered `SO_REUSEPORT`
  group (`udp::Bound`), and a `quic::Endpoint` built on it runs its demux and
  every connection driver on that worker, whichever handle built it. A member
  brings its slot along, so every issued connection id leads with the group's
  steering byte and the kernel keeps a connection (and a cluster dial's
  responses) on the worker that owns it. An endpoint on a dropped worker is
  refused.

Requires **Linux 6.12**; `Worker::new` refuses older kernels with a legible
error rather than degrading (note that default container seccomp policies
block io\_uring entirely). There is no fallback here: older kernels keep using
the tokio stack.

## Backends

The `quic` module uses the sans-IO [moq-noq-proto](https://github.com/moq-dev/noq)
stack with rustls. The `noq` feature is enabled by default and remains optional
so the worker, timers, and UDP socket can be built without QUIC.

| Feature | Stack | TLS |
|---|---|---|
| `noq` (default) | [moq-noq-proto](https://github.com/moq-dev/noq) | rustls |

Building without default features leaves the `quic` module out entirely.

```bash
cargo test -p moq-uring --features noq
```

`moq-relay` enables it with `--features io-uring`.

The `qlog` feature turns on noq's capture support. It writes one file per
connection.

## Validation

`tests/echo.rs` runs an echo against a tokio noq peer over the worker:
handshake, half a megabyte each way, and timers driven by noq's timeout.
`tests/session.rs` runs full moq-lite sessions through
`quic::Endpoint` (including two clients demuxed on one server socket), and
`tests/endpoint.rs` covers the endpoint mechanics (dial+accept on one socket,
version negotiation, the dial-only refusal), `tests/workers.rs` runs a
steered two-worker reuseport group serving one port across threads, and
`tests/web.rs` is WebTransport interop against `web-transport-moq`: stream and
datagram echo through the H3 framing,
close codes through the capsule, and a full moq-lite session over
WebTransport. All of them skip (loudly) below the kernel floor, which
includes GitHub-hosted CI runners.

## Benchmarks

`udp_tokio` and `udp_uring` are the disposable syscall-level matrices from the
first spike (recv batching x GRO x GSO, epoll vs io\_uring); see git history
for their methodology. `echo_noq` is the ablation matrix over the real
worker: the same noq echo with receive batching, GRO, and GSO toggled one
at a time.

```bash
just rs bench-udp --sample-size 20 --measurement-time 2 --warm-up-time 1
just rs bench-echo
```
