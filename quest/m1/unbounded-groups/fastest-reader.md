# [M] Fastest-reader pacing

## Goal

Every hop buffers at most a window of a group past its fastest reader, inside
the group's 32 MB tail, then stops reading the upstream QUIC stream, so stream
flow control pushes back hop by hop to the publisher. A hop with no reader
keeps reading and evicting, so pacing never blocks an unwatched track. A reader
that falls off the tail boards a
[flight](/quest/m1/unbounded-groups/flights.md) when a fill handler exists, and
otherwise refreshes or ends with `Error::Evicted`. A byte stream over MoQ then
loses nothing to a reader that keeps up, and each hop keeps its own congestion
control and retransmissions.

The paced hops include qmux sessions (WebSocket, `tcp://`, `tls://`,
`unix://`, and WebRTC): they lease receive credit natively, so incomplete
messages are bounded by the session's receive window and the lowest-priority
holder is evicted when it is stuck, as on native QUIC. The per-session memory
a peer can make us hold is one deliberate, documented number across native
QUIC and qmux. The browser and JS keep reading unpaced; their pacing is
[JS fastest-reader pacing](/quest/m2/js-fastest-reader.md), in m2.

## Plan

Decided 2026-10-25 (maintainer, mesh planning interview), reopening the
2026-10-24 "the live edge is never paced by a reader" decision for all tracks:

- Pace by the fastest reader, never the slowest: one slow reader must not stall
  the publisher or its other readers. Rejected: pacing by the slowest reader
  (TCP fanout semantics), and an opt-in flow-controlled track flag.
- Propagate with QUIC stream flow control per hop. No new wire messages, so it
  lands in the m1 cut beside front eviction
  ([moq-dev/moq#6993](https://github.com/moq-dev/moq/pull/6993)).
  Rejected: explicit MoQ-level credit messages.
- A reader past the window gets no new mechanism: it is an ordinary front
  eviction victim. Tracks without a fill handler, such as
  [byte streams](/quest/m2/stream/README.md), end that reader with
  `Evicted`, which a TCP proxy surfaces as a reset.

Decided 2026-10-28 (maintainer, after an audit of the plan):

- One 32 MB budget per group: the live tail that front eviction
  (`GroupState::trim` in `rs/moq-net/src/model/group.rs`) holds is
  "ahead" (bytes the fastest reader has not read, bounded by pacing) plus
  "behind" (bytes it has read, kept for slower readers). Shedding never
  passes the fastest reader, and the ahead window is at most 16 MB, half
  the tail, so a writer more than the window ahead waits instead of
  shedding unread bytes, and at least 16 MB always stays behind.
  The behind window then slides at the fastest reader's pace with no pacing
  of its own: a reader falls off once it trails the fastest by more than
  32 MB minus the ahead window. The ahead window is each hop's QUIC stream
  receive window, capped at 16 MB. The cap keeps
  [flight](/quest/m1/unbounded-groups/flights.md) admission sound: a reader
  admitted within the behind half is not shed when outstanding credit fills
  the ahead half.
- Reader progress is measured in bytes, not whole frames. Otherwise a frame
  larger than the ahead window deadlocks a writer against a reader that
  advances only on a finished frame. Until
  [byte holes](/quest/m1/unbounded-groups/byte-holes.md) gives Rust a
  chunked read, a whole-frame reader must not stall a paced writer.
- An origin producer that can wait (a byte stream) awaits a readiness future
  that stays pending until a reader exists and the ahead window has room.
  Live encoders ignore it and keep writing. Without the reader condition, a
  dialer writing before its listener subscribes would shed its first bytes
  once they passed 32 MB, and the stream would start with a hole. Relays
  keep the "no reader keeps reading and evicting" rule above, since they only
  hold groups some reader asked for.
- Cap the receive credit paused streams can hold. A paused stream keeps up to
  its stream window counted against the connection window, so enough paused
  groups (one per track with a slow reader) stall unrelated tracks on the
  same connection and trigger moq-quic's receive eviction
  (`CloseKind::Evicted`), which could reset the very byte stream pacing keeps
  lossless. Bring the mechanism to the maintainer before coding, and align it
  with the [receive pool](/quest/m2/receive-pool.md).
- Folded in 2026-11-01 (maintainer, follow-up of
  [moq-dev/moq#7328](https://github.com/moq-dev/moq/pull/7328)): the same cap
  covers streams parked waiting for TRACK_INFO. Group data that a SUBSCRIBE
  sent alongside TRACK, or a FETCH, leaves unread until TRACK_INFO arrives
  must not use up the connection receive window, or TRACK_INFO itself can't
  arrive. TRACK_INFO and other control messages always get connection
  credit, in Rust and JS. Test it: enough parked groups to fill the
  connection window, with TRACK_INFO still arriving and the groups then read.

Decided 2026-10-30 (maintainer, in the
[moq-dev/moq#7388](https://github.com/moq-dev/moq/pull/7388) iteration):

- moq-net caps the receive credit paused groups hold per session at about half
  the connection window, read live from the transport
  (`poll::Session::receive_window`). A group that would push the total past
  the cap keeps reading and shedding instead of pausing. This stands until the
  [receive pool](/quest/m2/receive-pool.md) folds paused credit into its
  per-worker budget.
- The readiness future is `group::Producer::poll_ready` and `async fn ready()`,
  pending until a reader exists and the 16 MB window has room.

Prior art splits the same way (2026-10-28 survey): Varnish streaming, ATS
read-while-writer, and WHATWG `tee()` fan out at the writer's pace and buffer
everything; Varnish `transit_buffer`, nginx unbuffered proxying, and ATS flow
control pace a single client; Node `pipe()` to several destinations paces the
slowest. This is `tee()` with a bounded buffer that drops the branch that
falls off it ([whatwg/streams#1235](https://github.com/whatwg/streams/issues/1235)).

Landed in [moq-dev/moq#7388](https://github.com/moq-dev/moq/pull/7388) (Rust,
session ingest only): readers report the byte end of each frame they take
(`Pace` in `rs/moq-net/src/model/pace.rs`), and the shared transport reader's
payload loop (`coding::Reader::poll_read_frame`) waits on
`group::Producer::poll_paced` before taking more, and admits a frame only once
its declared size fits the window (`coding::Reader::poll_admit`), so moq-lite
and moq-transport ingest both pause past the 16 MB window. A reader that took every
committed frame counts as waiting on the writer, so a frame larger than the
window never stalls it. A paused stream is not read, so it is watched for a
reset instead (`poll::RecvStream::poll_reset`, which moq-quic answers through
moq-tokio and moq-uring without taking bytes). Tests cover one hop on lite-06,
lite-07-wip, and moq-transport-22: an idle reader holds the relay copy near the
window and then receives every frame, a slow reader ends `Evicted` while the
fast one keeps up, a session past its credit cap reads on, and a publisher's
reset ends a paused relay copy. The same PR landed the credit
cap (each paused stream reserves its stream window from a per-session budget
in `rs/moq-net/src/transport/boxed.rs`; moq-quic reports its windows through
moq-tokio and moq-uring, and a transport that reports none never pauses) and
the public readiness future.

Decided 2026-11-02 (maintainer, /quest-grill interview on
[moq-dev/moq#7624](https://github.com/moq-dev/moq/pull/7624)):

- Native only: finish the moq-quic path (tokio, io_uring, and qmux on its
  mux). Browser parity waits on a real receive-credit contract for
  WebTransport, since `Reader.closed` watches resets but nothing says how much
  credit a paused stream holds; guessed window constants would move memory
  outside flow control. Rejected: a browser read-ahead cap, and dropping the
  browser from scope.
- qmux is handled here, absorbing the m2 qmux receive-lease quest: qmux's
  `RecvStream` leases natively on moq-quic's mux instead of through
  `poll::Buffered`, evicts by priority, and so closes the 2026-10-27 audit's
  NET-3 gap (`Buffered` bounded each stream's incomplete message by the largest
  message, and nothing bounded them in aggregate). Rejected: deferring qmux to
  that quest, and adding hooks here while keeping leases in m2.
- The lease floor is one 1 MiB segment for every transport and version: the
  lite-01/02 `ANNOUNCE_INIT` cap drops from 64 MiB to a segment less its 16
  bytes of prefixes, so `moq_net::transport::MIN_RECEIVE_WINDOW` is 1 MiB. A
  legacy origin whose initial set exceeds it fails its announce loudly, on
  both ends. Rejected: a 4 MiB cap, and keeping 64 MiB with a per-version
  floor.
- Window defaults are one per-session number across native QUIC and qmux:
  clients get a 4 MiB connection and a 1 MiB stream receive window, and qmux
  takes the same client/peer split as native QUIC. The cluster peer windows
  (`[limits.peer.quic]`, 640 MiB) and the client `send_window` (16 MiB) are
  re-derived from measured bandwidth-delay needs, with the maintainer
  suggesting about 16 MiB for peers. Rejected for clients: 16, 32, or 128 MiB
  connection windows, and a 4 MiB stream window.
- Local readiness takes the next frame's `frame::Info`
  (`group::Producer::poll_ready(waiter, &info)` and `ready(&info)`), reusing
  ingest's admission semantics, including its whole-frame deadlock exception.
  Rejected: a bare size on the same methods.
- Whole-batch fills move to [flights](/quest/m1/unbounded-groups/flights.md),
  since the failure crosses groups while this mechanism paces one group copy.

The adapter inventory this plan once listed is stale: moq-tokio has no Quinn
backend, and moq-ffi drives sessions through moq-tokio rather than a
transport of its own, so neither needs an adapter. Iroh, which returns credit
as it reads, leaves with its removal
([moq-dev/moq#7345](https://github.com/moq-dev/moq/pull/7345)); until then its
ingest never pauses.

Decided 2026-11-02 (maintainer, settling the open items of
[moq-dev/moq#7624](https://github.com/moq-dev/moq/pull/7624)):

- Peer windows stay at 16 MiB receive and send in `[limits.peer.quic]`, with
  the 1 MiB stream window from `[quic]`. Rejected: 32 MiB, and a separate
  per-stream peer window.
- The client `send_window` is 4 MiB, matching its receive window. Rejected:
  keeping 16 MiB.
- A GROUP stream waiting unread for its TRACK_INFO reserves nothing from the
  paused budget: it waits at the lowest receive priority, so a full window
  evicts it before control streams, and TRACK_INFO still gets credit. This
  replaces the 2026-11-01 wording that the paused cap covers such streams.
  Rejected: stopping it once the budget is full.
- Every qmux session a server accepts takes the configured `[quic]` windows,
  including the relay's own WebSocket listener. Rejected: leaving them at the
  defaults until admission.
- Split: this PR lands the windows, the floor, qmux leases, sized readiness,
  TRACK_INFO priority, and the accept paths, and the rest stays here.
  Rejected: finishing everything in one PR.

Landed in [moq-dev/moq#7624](https://github.com/moq-dev/moq/pull/7624):
sized local readiness; the one-segment floor; 4 MiB, 1 MiB, and 4 MiB client
receive, stream, and send windows in moq-tokio, moq-uring, and qmux; 16 MiB
cluster peer windows; native qmux leases with eviction, window reporting, and
`poll_reset`; qmux sessions starting at the server's `[quic]` windows on every
accept path and widening to `[limits.peer.quic]` for peers; and a Bun sender
exhausting a qmux receiver over WebSocket (`just test receive-credit`). At the
client defaults a session pauses two group streams (half the window, a stream
window each), and a peer link eight; past that, groups read and shed as
before pacing. The same PR ranks streams waiting on TRACK_INFO (lite GROUP
streams and FETCH answers, and ietf data streams waiting on their alias)
below every other stream, and they watch `poll_reset`, which now reports an
eviction, so an evicted stream is stopped and eviction resumes.

Evidence added in the follow-up attempt (2026-11-02):

- `two_relays_bound_their_cache_and_resume_without_a_hole` carries one 64 MiB
  group through two session relays on lite-06, lite-07-wip, and
  moq-transport-22. It parks the final reader, measures each relay's cache pool
  charge, checks backpressure reaches a readiness-driven origin, then verifies
  every payload byte and clean completion. The upstream cache includes sent
  history within its 32 MiB tail; the downstream cache holds near 16 MiB.
  These are cache charges, not transport buffers or process RSS.
- `rs/qmux/benches/receive.rs` measures live requested heap bytes and verified
  payload throughput at 4 MiB connection, 1 MiB stream, and 4 MiB send windows.
  It independently sweeps TCP/WebSocket framing over bounded byte pipes,
  qmux-00/01/02, 1/8/64 sessions, and 1/8 streams per session. Eight unread
  512 KiB leases fill each receiver's connection window. Heap includes both
  endpoints, drivers, stream tables, pipes, and receive backings, excluding
  the runtime and shared source payload. Nightly's supplemental Rust leg runs
  it. All 36 cases completed locally on the shared host. With 64 sessions and
  eight streams each, live heap per session was 4,506,154-4,506,265 bytes for
  TCP framing and 4,671,843-4,671,979 bytes for WebSocket framing across the
  three versions, including both endpoints. Throughput point estimates were
  2.75-3.85 GiB/s and 2.41-3.54 GiB/s respectively, with concurrent builds and
  checks. These are observations, not a quiet-host regression verdict.
  This isolates receive ownership; the broader MoQ transport/version
  scenarios remain in [their quest](/quest/m2/bench-transport-sweep.md).
- `websocket_upgrade_advertises_the_configured_receive_windows` reads actual
  qmux transport parameters through the relay's own web listener and
  `serve_ws`, before MoQ admission, for qmux-00/01/02. Nondefault operator
  windows and stream limits must reach all advertised receive parameters.
  No new public inspection API is needed.

What remains:

- A quiet-host comparison against the base of
  [moq-dev/moq#7388](https://github.com/moq-dev/moq/pull/7388) for
  `group_read_frames`, `group_contended`, and `group_relay_frames`, plus
  `group_paced` (a writer parked at the window, over 1, 8, and 64 readers).
  The cursor's report is one relaxed load per frame or batch, and a
  `fetch_max` only for the fastest reader. This attempt shares the host with
  concurrent CI/Nix work, so its throughput measurements are descriptive,
  not this comparison. The first implementation's busy-host repeats swung
  by up to 50% either way.
- Resolve the earlier ahead-window wording against the landed source:
  `model::pace::AHEAD` and `group::Producer::poll_paced` use a fixed 16 MiB
  cache pacing window, even with 1 MiB transport stream credit. The credit
  reported by `receive_window` sizes the paused-credit reservation, not the
  cache pacing threshold. The two-relay test records this current behavior;
  it does not establish the earlier promise that ahead is capped by stream
  receive credit.

Decided 2026-11-02 (maintainer, follow-up planning after the 2026-11-02
/quest-complete run): JS parity moves to
[JS fastest-reader pacing](/quest/m2/js-fastest-reader.md) in m2, which waits
on [browser receive credit](/quest/m2/browser-receive-credit.md), so the m1
cut does not wait on browsers.

## Waiting

- Maintainer: provide a host without concurrent workloads, then run the paired
  group benchmarks above against the pre-pacing base with repeated alternating
  runs. Keep this quest and its PR draft until that comparison is recorded.
- Maintainer: settle the ahead-window discrepancy: keep the decided
  transport-sized cache window (recommended, then update ingest and its tests),
  or accept the landed fixed 16 MiB cache window and correct the earlier Plan.
