# [M] Relay egress syscall and allocation profile

## Goal

A measured verdict on the relay's egress costs below moq-net: syscalls per
train, allocations and copies per frame, and whether a released GSO train
bursts at the bottleneck. Each lever below is built only if the profile
shows its cost; a lever measured within noise is recorded and closed.

## Plan

Decided in the 2026-09-30 audit: send batching, kernel pacing, and send
buffer pools merged into this one measure-first quest, since all three are
questions about the same egress path and share one profile.

Decided 2026-10-30 (maintainer): the profile also covers moq-quic's
ingress and egress (`just bench-profile cpu` on `io-uring-workers`), since
[quic-io-boundary](/quest/m5/quic-io-boundary.md) waits on it to rank its
candidates.

Measure first, on the chat and fanout shapes with many connections, tokio
workers versus io_uring workers, with the existing profiling captures:

- Syscalls: the tokio path sends one `sendmsg` per GSO train per connection;
  the io_uring path queues one SQE per train and submits them together. Count
  syscalls per second and CPU per Gbps, and confirm from the io_uring
  worker's `enters` counter that the ring already amortizes.
- Allocations: `moq-quic`'s `SendBuffer` keeps each write as the caller's
  `Bytes`, with no coalescing copy. Count allocations and bytes copied per
  frame above it, split by frame size, so any copy left is located.
- Bursts: the QUIC connection paces inside `poll_transmit` on both runtimes
  (#4400; `moq-congestion`'s pacer), so the only open question
  is whether a released GSO train leaves the NIC as a burst the bottleneck
  cannot absorb. On a netem bottleneck, compare inter-packet gaps and queue
  occupancy for a 64-segment train against the same bytes paced at the
  controller's rate.

Then, only where the profile shows a cost:

- `sendmmsg` in `moq_sock::udp` behind the existing GSO path, submitting every
  train ready across connections in one call, falling back to per-train
  `sendmsg` on partial failure the way the GSO fallback does. A contributor's
  prototype on the tokio driver gives the profile a starting point; none of
  its code is in-tree, and its numbers below are the prototype's, not the
  profile's. It was measured before being
  [ported onto main in a fork](https://github.com/zshenker/moq/commit/040df9948bd110a83463f35cd4bd86245db03373)
  (opt-in through `Endpoint::batch_sends`), and comes with its
  [raw samples](https://github.com/zshenker/moq/blob/eae2ca6941ce67b8e14ba6afdb76c1c2240c45a0/bench/driver-batch-sends/samples.jsonl),
  so the profile should re-measure the port.
  - Revision: measured on e072cf604 with #7458, #7418, #7394, #7543, and a
    GSO byte cap equivalent to #7649 applied. The prototype revisions measured
    predate the handshake bypass, and the last round ran after the drop and
    rebind fixes. The steady-state send path matches the port, which differs
    in its opt-in (`Endpoint::batch_sends`), its flush task's spawner, the
    handshake bypass, and its base. Those revisions and the harness are not
    public.
  - Invocation: one run per sample of the harness's
    `bench --sessions 1000 --rate 30 --object-size 1024-4096 --objects-per-stream 1 --client-threads 6 --client-endpoints 12 --pin --batch-sends {false|true}`,
    with audio at `--rate 50 --object-size 100-200`, 1 Mbit/s video at
    `--object-size 3125-5208 --objects-per-stream 30`, and 3 Mbit/s video at
    `--sessions 500 --object-size 9375-15625 --objects-per-stream 30`. Its
    defaults are a 1,280-byte payload, GSO on, one tokio worker, a 5 s warmup,
    a 15 s measure, 128 handshakes in flight, and a 15 s keep-alive, all under
    `taskset` on physical cores.
  - Setup: server and client on one host over loopback, the server on one
    tokio worker pinned to one physical core, and the client on 6 pinned workers
    over 12 sockets. 1,280-byte payloads, MTU discovery off, GSO and GRO on,
    BBR3. Only the server batches. Each sample is one 15 s run after a 5 s
    warmup, on a dual-socket Broadwell Xeon with Linux 6.12. On and off ran back
    to back on the same binary, in alternating order across runs.
  - Server CPU (`getrusage`, % of one core):
    - small objects (1,024 to 4,096 B at 30/s, 1,000 sessions): 61.7% off
      against 54.9% on, n=3 each, so 8 to 15% lower;
    - audio-rate (100 to 200 B at 50/s, 1,000 sessions): 66.9% against 53.9%,
      n=3 each, so 16 to 22% lower;
    - 1 and 3 Mbit/s video: 10% and 8% lower, n=1 each.

    The system share fell from 31 to 38% to 22 to 34%. A single run varies by
    about ±3%.
  - Calls: in separate 5 s `strace` windows of the small-object point, the
    unbatched server made 135,497 `sendmsg` and the batched one 7,214
    `sendmmsg` plus 506 `sendmsg`, so about 18 to 19 trains per `sendmmsg`.
    That is an estimate across runs with no direct counter, and `strace` slows
    each arm differently.
  - Latency (per object, from the server's write to the client's read of the
    last byte): p50 rose from 36 to 72 µs to 258 to 352 µs. On the small-object
    and audio points, p99 rose from 104 to 705 µs to 606 to 770 µs.

  **End to end, through a QUIC reverse proxy.** The same prototype, measured as
  one server shard behind a QUIC reverse proxy. Clients connect to the proxy,
  and the proxy connects to the server over loopback with an 8,192-byte UDP
  payload. Clients are moq-bench fan-out viewers: audio-rate, then 3 Mbit/s
  video, 5 min each. The four runs ran in ABBA order (off, on, on, off), one
  run per cell, so no load's comparison is counterbalanced. A near-repeat of
  off-300 put CPU within about 1% and audio p99 about 8 ms apart. Server CPU is
  in cores, as mean / max.
  - Build: the server ran the prototype in a private downstream build, not
    public, with batching switched on per run through an opt-in on its endpoint
    (`Endpoint::batch_sends` in the linked port). The clients were moq-bench at
    337f794 plus one local patch, with each run's first 30 s skipped.
  - Proxy: it terminates QUIC and MoQ, and opens one upstream session per client
    session to the server, each on its own UDP socket (602 upstream sessions for
    600 viewers at 400/200). So the server's egress is still per-viewer fan-out,
    the cost this lever targets. Only the server batched; the proxy was
    unchanged.

  | Load (audio / video viewers) | Batching | Audio p50 / p99, lost groups | Audio CPU | Video p99 |
  |---|---|---|---|---|
  | 300 / 150 | off | 11 / 38 ms, 6,090 | 0.492 / 0.596 | 21 ms |
  | 300 / 150 | on | 13 / 54 ms, 9,781 | 0.490 / 0.560 | 26 ms |
  | 400 / 200 | on | 27 / 110 ms, 150,424 | 0.596 / 0.695 | 79 ms |
  | 400 / 200 | off | 27 / 212 ms, 325,694 | 0.688 / 0.779 | 31 ms |

  - **Below the server's ceiling, batching saved no CPU and raised p99.** The
    loopback harness's 8–22% saving did not carry over, and why hasn't been
    measured. Both arms lost groups at this point, batching more (9,781 against
    6,090), so the p99 rise is measured under unequal loss, not drop-free.
  - **Only at 400 audio did it cut CPU, by 13%.** Lost groups fell too (150,424
    against 325,694), but the proxy's listener dropped about 1.1 M datagrams in
    both runs, so that difference can't be credited to batching. The server was
    past its limit: with batching off its own socket dropped 3,100 datagrams,
    against none with it on, and CPU peaked at 0.78. Audio p99 fell from 212 to
    110 ms, while video p99 rose from 31 to 79 ms.
  - **The losses at 300 came from the proxy's client-facing socket buffer**, not
    the server: it dropped 242,019 datagrams with batching off and 232,662 with
    it on, at its 208 KiB default. With a 1 MiB receive buffer, batching off
    loses nothing there (audio p50 / p99 11 / 29 ms, CPU 0.499 / 0.609, video
    p99 22 ms); batching on was not re-run with it.

  So on this path, below the ceiling p99 rose with no CPU saving; past it, CPU
  fell and audio p99 halved but video p99 rose. That leans toward keeping batching
  opt-in and off by default, if it ships, on single runs.

  Its shape:
  - `UdpSocketState::try_send_many` on Linux sends up to 64 transmits, one
    `mmsghdr` each, in one `sendmmsg`, each with its own GSO segment size,
    ECN, and source-address cmsgs. It returns how many the kernel took, and
    retries an `EIO` or `EINVAL` on the first transmit alone through
    `try_send`, so GSO halting still applies.
  - The driver copies each train into a per-socket queue, bounded in bytes,
    and one task per socket yields once and then flushes the queue.
  - Handshake packets try the socket directly, since `handle_unreachable`
    acts only during a handshake and a queued send's error never reaches it;
    without that, a dial to an unreachable address would wait out the
    handshake timeout instead of failing with `NO_VIABLE_PATH`. The bypass is
    not unconditional: on `WouldBlock` the port queues the handshake packet,
    and that packet's send error is lost the same way. A shipped version
    should keep backpressure on handshake sends instead of queueing them.
  - A rebind wraps the new socket the same way. Dropping the queue sends what
    the socket has room for and closes it synchronously, freeing the port. The
    flush is best-effort: it stops at `WouldBlock`, and a batch the flush task
    already took is dropped once the socket closes, so a CONNECTION_CLOSE can
    be lost. A shipped version should drain that batch too.

  Open: whether batching is opt-in in the driver's config or on by default,
  given the added p50 and p99 latency, and whether moq-uring, which already
  submits every train in one ring enter, needs any of it.
- A `BufFactory`-style seam on `SendBuffer` in `moq-quic` that takes buffers
  from a pool sized from the send window and returns them on ACK. Design it
  with [the I/O boundary redesign](/quest/m5/quic-io-boundary.md), which
  reworks the same transmit buffers.
- `SCM_TXTIME` stamps, one per train, at the time the pacer would have
  released it (the next-send time `moq-congestion`'s trait exposes), tested under both `etf` and `fq`. The socket is shared across
  connections per worker, so `SO_MAX_PACING_RATE` cannot express
  per-connection pacing. Measure on a real NIC as well as loopback, since
  `etf` needs hardware offload to be exact.

Report CPU per Gbps, RSS, loss, and p99 latency on Linux: `just bench BASE`
for tokio, and `just bench-runtime` at both revisions for the ring, since
`just bench BASE` runs only tokio. The verdict names which levers ship and which qdisc, if any, relay
hosts need.

## Related

- [QUIC I/O boundary](/quest/m5/quic-io-boundary.md) - redesigns the transmit buffers the pool lever would feed
- [#3201](/quest/m5/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) -
  zero-copy on the same trains
- [#3204](/quest/m5/3204-moq-uring-register-tx-pool-buffers-for-zero-copy-sends.md) -
  the UDP-side pool the same buffers could feed
