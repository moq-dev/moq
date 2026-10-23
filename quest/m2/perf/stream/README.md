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

- Nothing measures WebSocket or TCP throughput today. The moq-uring benches
  are UDP and QUIC only, and `rs/moq-relay/benches/http_idle.rs` measures idle
  accepts.
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
    `BytesMut` per outbound size varint
- qmux is hard-wired to tokio: `tokio::spawn`, `tokio::sync::mpsc`,
  `tokio::select!`, and `tokio::time` for the WebSocket keep-alive. That is
  why it cannot run on an io_uring worker today, and why the driver is a
  prerequisite of the ring port rather than an optional win.
- WebSocket framing is message-oriented: one Binary message is one qmux
  record (one frame on qmux-00), at most `max_record_size` (16382 by
  default), and Text is a protocol violation. The browser
  (`js/qmux/src/web-socket-stream.ts`) masks every frame and picks its own
  fragmentation, so a server still unmasks and reassembles continuations.

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

## Required

- [Bench](/quest/m2/perf/stream/bench.md) - WebSocket, WSS, and TLS qmux relay workloads plus a qmux micro-bench attribute stream-transport CPU to its layers
- [Driver](/quest/m2/perf/stream/driver.md) - a qmux session is one caller-spawned task polling the socket it owns, with no split, pump, channel, or per-record allocation
- [Ring TCP ablation](/quest/m2/perf/stream/ablation.md) - measured ring TCP against tokio TCP under the qmux workload picks the mechanisms the port uses
- [Ring TCP](/quest/m2/perf/stream/ring.md) - moq-uring serves TCP, and hyper (therefore axum) runs on it
- [Relay listeners on the workers](/quest/m2/perf/stream/relay.md) - the relay's WebSocket, qmux TCP, and HTTP listeners run on the io_uring workers
- [Codec](/quest/m2/perf/stream/codec.md) - qmux frames WebSocket itself and tungstenite leaves the tree, if the bench says it pays

## Related

- [WebSocket rate caps](/quest/m2/rate-websocket.md) - its IO wrapper sits under the protocol, on the socket the driver now owns
- [Stream sockets disable Nagle](/quest/m1/tcp-nodelay.md) - the latency fix on the same sockets, landing first
