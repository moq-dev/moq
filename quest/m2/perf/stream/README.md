# Stream transports

## Goal

Cut relay CPU per message and per byte for qmux sessions over WebSocket and
TCP, with no wire change. First measure the cost and attribute it to layers.
Then qmux drives a socket it owns from one task. Then the relay serves those
sockets from the io_uring workers, where io_uring's advantage over epoll is far
larger for TCP than for UDP. Tokio stays the relay's control-plane runtime:
auth, iroh, cert reload, signals, and session supervision.

Out of scope: h2 or h3 WebSocket, permessage-deflate, and any wire change.

## Plan

Facts from the 2026-10-22 audit, so quests don't re-litigate them:

- `just bench` runs `chat` and `fanout` over QUIC and qmux over `ws://`,
  `wss://`, `tcp://`, and `tls://`, reporting relay CPU per delivered frame and
  per Gbps; `just bench-profile` takes the transport as its last argument. The
  qmux micro-bench (`rs/qmux/examples/bench.rs`) echoes one message at a time
  over loopback, swept over transport x sessions x message size, and reports
  process CPU per message, which is one record up to the 16382-byte default
  maximum record size. `bench/README.md` covers both.
- The relay's WebSocket path is axum (hyper, `axum-server`, tokio-rustls),
  then axum's tungstenite 0.29, converted zero-copy into qmux's tungstenite
  0.30 (`rs/moq-relay/src/websocket.rs`). The relay therefore compiles two
  tungstenite versions. qmux re-exports `tokio_tungstenite` and `tungstenite`
  in its public API (`rs/qmux/src/ws.rs`).
- tungstenite itself costs a zero-filled 128 KiB read buffer, a copy into
  its `out_buffer` on write, and a memmove after a partial write. The layers
  qmux adds around it look heavier, though none is measured yet:
  - on WebSocket, a `futures` split BiLock shared by reader and writer, a
    `ws_pump` task, and an `mpsc(16)` channel (`rs/qmux/src/transport.rs`)
  - a `run_reader` task taking the session `Mutex` per record
    (`rs/qmux/src/session.rs`)
  - a new `Vec` per outbound record from `mux::fill`
  - on plain TCP, a `BytesMut::zeroed` allocation per inbound record and a
    `BytesMut` per outbound size varint, behind the same shape as WebSocket:
    `tokio::io::split`, a `reader_loop` task, and an `mpsc` channel
- qmux is hard-wired to tokio: `tokio::spawn`, `tokio::sync::mpsc`,
  `tokio::select!`, and `tokio::time` for the WebSocket keep-alive. That is
  why it cannot run on an io_uring worker today, and why the driver is a
  prerequisite of the ring port rather than an optional win.
- WebSocket framing is message-oriented: one Binary message is one qmux
  record (one frame on qmux-00), at most `max_record_size` (16382 by
  default), and Text is a protocol violation. The browser
  (`js/qmux/src/web-socket-stream.ts`) masks every frame and picks its own
  fragmentation, so a server still unmasks and reassembles continuations.

Measured 2026-10-28 on the 24-core `desktop`, with other agents running
(load near 10), so read shares rather than absolute numbers. Shares come from
`perf` at 999 Hz on profiling builds with frame pointers, each sample charged
to the innermost layer that owns it; syscalls and allocator calls are charged
to themselves.

qmux micro-bench, 1024 B echo, 16 and 256 sessions:

| Transport | CPU per record | Syscalls | tungstenite | TLS | qmux session | moq-quic mux | tokio | split | mpsc | allocator |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| tcp | 2.4 / 3.2 us | 52 / 52% | | | 16 / 18% | 10 / 10% | 12 / 10% | 1 / 1% | 3 / 3% | 2 / 2% |
| tls | 3.1 / 5.1 us | 44 / 44% | | 16 / 13% | 14 / 14% | 9 / 9% | 9 / 9% | 1 / 2% | 2 / 3% | 2 / 2% |
| ws | 4.5 / 10.0 us | 33 / 26% | 32 / 43% | | 11 / 9% | 8 / 5% | 8 / 7% | 4 / 4% | 1 / 2% | 1 / 1% |
| wss | 5.2 / 15.1 us | 30 / 27% | 29 / 32% | 11 / 7% | 9 / 11% | 6 / 7% | 7 / 5% | 3 / 4% | 1 / 3% | 3 / 1% |

Relay `chat` (one publisher, 2,000 viewers, 20,000 frames/s delivered) on 4
Tokio workers: QUIC costs 94 us of relay CPU per delivered frame, `ws` 33,
`wss` 35, `tcp` 30, and `tls` 31. Over the stream transports, moq-net takes
36-41% of relay CPU, kio 19-22%, syscalls 14-16%, moq-quic 7%, the qmux
session 6-7%, tungstenite 5% (ws and wss), TLS 2-3%, the allocator 3%, the
split at most 2%, and `mpsc` under 0.5%.

What that says, layer by layer:

- **tungstenite** is the largest transport cost on WebSocket, and almost all of
  it is one thing: its read buffer is resized with zeros to its full capacity
  (`read_buffer_size`, 128 KiB) on every read attempt, including one that
  returns `WouldBlock`. That is 29-43% of the micro-bench and grows with the
  session count, but 5% of relay `chat`, where only the publisher's socket is
  read. Its `out_buffer` copy and memmove do not register.
- **The split, pump, channel, and mutex hops** are small: the BiLock 3-4% on
  WebSocket, `tokio::io::split` 1-2% on TCP, `mpsc` 1-3%, and the session
  `Mutex` never contends on one thread, so it shows only inside the session's
  own 9-18%.
- **Per-record allocations**: the allocator is 1-3% everywhere.
- **TLS**: 7-16% of the micro-bench, 2-3% of relay `chat`.
- **Syscalls** are a quarter to a half of the micro-bench, where every record
  costs a send and a read, and 14-16% of relay `chat`. That is the ring's
  target.

Decisions (2026-10-22), each with its reason:

- **Measure, then decide.** The bench lands first, and every quest records a
  before/after against it, per the perf line's rule.
- **qmux owns the socket, no wire change.** The win comes from removing the
  split, pump, channel, and per-record allocations, not from swapping one
  codec for another.
- **The driver goes ahead without a gate.** It is the ring port's
  prerequisite. It is still measured.
- **The codec waits for a verdict.** It replaces tungstenite only if the
  bench attributes a material share to tungstenite once the driver has
  landed. tungstenite runs over any tokio-IO stream, the ring's included, so
  the codec is not on the ring's critical path.
- **The ring line moved in from m3.** io_uring pays far more for TCP than for
  UDP, so it no longer waits on fleet demand. Its ablation picks mechanisms;
  it no longer gates the line. This is a deliberate exemption from the perf
  line's "a measured no-win abandons the quest" rule (reaffirmed in review):
  the ring is where the relay's media path is heading, and putting stream
  sessions on the same pinned thread as QUIC ones is the goal in its own
  right. The before/after is still recorded.
- **The ablation reuses the bench harness** rather than building a second
  one.
- **Ranked second in m2/perf**, after kio channel contention, which is the
  largest cost measured so far.

Decided 2026-10-31 (maintainer), in
[moq-dev/moq#7086](https://github.com/moq-dev/moq/pull/7086):

- **`just bench` keeps all five transports by default, `tls` included.** A base
  without `listen.tcp.tls`, such as `origin/release` today, fails at its first
  `tls` case, so pass `"quic ws wss tcp"` until a release includes it
  (`bench/README.md`). Probing the base relay first would mean building it
  before Criterion, and a QUIC-only default would hide the transports this line
  measures.
- **The longer bench is accepted.** The relay phase grew from 2 cases to 11,
  about 15 minutes against a base at 3 rounds; the transport argument narrows a
  run when that matters.

## Required

- [Driver](/quest/m2/perf/stream/driver.md) - a qmux session is one caller-spawned task polling the socket it owns, with no split, pump, channel, or per-record allocation
- [Ring TCP ablation](/quest/m2/perf/stream/ablation.md) - measured ring TCP against tokio TCP under the qmux workload picks the mechanisms the port uses
- [Ring TCP](/quest/m2/perf/stream/ring.md) - moq-uring serves TCP, and hyper (therefore axum) runs on it
- [Relay listeners on the workers](/quest/m2/perf/stream/relay.md) - the relay's WebSocket, qmux TCP, and HTTP listeners run on the io_uring workers
- [Codec](/quest/m2/perf/stream/codec.md) - qmux frames WebSocket itself and tungstenite leaves the tree, if the bench says it pays

## Related

- [WebSocket rate caps](/quest/m2/rate-websocket.md) - its IO wrapper sits under the protocol, on the socket the driver now owns
