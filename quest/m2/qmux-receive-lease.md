# [M] qmux leases receive memory

## Goal

qmux sessions (WebSocket, `tcp://`, `tls://`, `unix://`, and WebRTC) bound
what a peer's incomplete messages hold by the session's receive window, and
evict the lowest-priority holder when that window is stuck, as native QUIC
does after [moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878). The
per-session memory a peer can make us hold is a deliberate, documented number,
chosen after weighing its cost against today's.

## Plan

Starts after [moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878)
merges (open on 2026-10-22). Re-read the landed code before designing, since
the lease API may still move.

State with #5878: qmux's `RecvStream` wraps its reader in moq-net's
`poll::Buffered`, which returns connection credit as it reads. `max_data`
then bounds only unread bytes. Each stream's incomplete message is bounded by
the largest message alone, so nothing bounds them in aggregate and nothing is
evicted. moq-tokio's `qmux_config` grants 10,000 streams per direction and
keeps qmux's defaults of 1 MiB `max_data` and 256 KiB `max_stream_data`.
qmux already runs on moq-quic's mux `Connection`, whose stream state carries
the lease and eviction machinery the tokio and io_uring drivers use, so the
transport side should be wiring more than new machinery.

The cost: a leasing transport must offer at least
`moq_net::transport::MIN_RECEIVE_WINDOW`, the lite-01/02 `AnnounceInit` cap
(64 MiB) plus 16 bytes of prefixes, which also covers the 32 MiB frame cap.
Native QUIC defaults to 128 MiB. qmux keeps reading its byte stream to see
control frames, so the window, not the socket, is what bounds the bytes a
slow reader's peer parks in userspace. Going from 1 MiB to 64 MiB multiplies
that by every browser fallback, TCP, and WebRTC session a relay serves.

Bring the options to the maintainer with a recommendation before building:

- **The native floor.** Lease at `MIN_RECEIVE_WINDOW` or native's 128 MiB.
  Simplest, and one behavior across transports, at the largest allowance.
- **A smaller qmux floor.** Only lite-01/02 needs the 64 MiB term, and
  [Segmented frames](/quest/m2/unbounded-groups/segmented-frames.md) removes
  the frame term. MAX_DATA can only grow, so a session can start at the floor
  its version needs and extend once SETUP (or a WebSocket subprotocol) names a
  legacy version. A floor below some supported unit must refuse it loudly.
- **Grow on demand.** Advertise a small window and extend MAX_DATA only when
  leases block progress, up to the floor, evicting only at the floor. Typical
  sessions stay small. Check that ordinary media adds no round trips.

Bounding `Buffered` in aggregate with a cap that only refuses is not an
option: it gives no backpressure. Lowering a protocol or frame size limit to
shrink the floor is a separate maintainer decision.

Validate the configured windows against whatever floor is chosen and fail
loudly, as moq-tokio's `ReceiveWindowTooSmall` does, and align moq-tokio's
`qmux_config` and WebRTC config with it. Cover the native receive-credit
cases over qmux: many partial messages filling the window, eviction by
priority, and progress after eviction, with a JS sender over WebSocket.
Measure retained bytes per session and throughput across many sessions for
the chosen window.

Public API: qmux's `RecvStream` leases natively instead of through
`Buffered`, and its window defaults or validation change (MIGRATE.md). Wire:
QMux MAX_DATA and STOP_SENDING only, no MoQ format change.

## Related

- [Segmented frames](/quest/m2/unbounded-groups/segmented-frames.md) - frame credit returns per segment, shrinking the floor's frame term
- [WebSocket caps](/quest/m2/rate-websocket.md) - paced reads on the same sessions, inside the window
- [Transport x version benches](/quest/m2/bench-transport-sweep.md) - where a per-session memory and throughput sweep belongs
