# User-space TCP

## Goal

A relay can serve one TCP listener port from a user-space TCP stack on AF_XDP.
That stack sends with moq-congestion's controllers and shared pacer, as
`moq-quic` does, and builds QMux frames only when a segment leaves. A stream
reset or expired before then never reaches the wire. Bytes already sent are
retransmitted as TCP requires. Every peer, browser WebSocket viewers included,
still speaks plain TCP, so this changes the relay's egress (the server is the
sender) and nothing on the wire.

It beats the kernel baseline (`TCP_NOTSENT_LOWAT` at the congestion window,
[moq-dev/moq#9242](https://github.com/moq-dev/moq/pull/9242)) on stale bytes
sent and on how well its estimate tracks the path, or it doesn't ship.

Non-goals: clients dialing over it, DPDK, CPU savings, and what a peer's own
kernel TCP sends us.

## Plan

Decided 2026-11-19 (maintainer, /quest-plan):

- ✅ Kernel `TCP_NOTSENT_LOWAT` first, then this epic, which must beat it
  (rejected: this epic only; the kernel step only). The kernel can't run
  moq-congestion, so `bandwidth_estimate`, probing, and the realtime
  controllers ([quic-realtime](/quest/m5/quic-realtime.md)) never reach TCP
  sessions without this epic.
- ✅ AF_XDP (rejected: TUN, which needs DNAT and costs a syscall and copy per
  packet; raw sockets with an nftables RST drop, where the kernel still parses
  every packet; DPDK, which takes the NIC from the OS). The XDP program
  redirects only the listener's port and passes everything else (SSH, ARP) to the
  kernel, which never sees the steered packets, so it sends no RST.
- ✅ Our own sans-IO TCP connection in `moq-quic` (rejected: smoltcp, whose
  congestion control is a closed enum, which has no SACK recovery, and which
  copies data into its own send buffer; a smoltcp fork, a sender rewrite that
  also tracks a diverged upstream). It sits beside the QMux mux and shares the
  `Controller`, pacer, and sent tracking.
- ✅ The whole listener port (rejected: a dedicated QMux port). XDP can't tell
  protocols apart at SYN, and browser `wss://` arrives on 443
  ([one port](/quest/m3/one-port/README.md)). QMux gets late commit; hyper, axum,
  RTMP, and HLS run over an `AsyncRead`/`AsyncWrite` stream on the same stack and
  still get moq-congestion.
- ✅ The relay accepts only (rejected: native clients dial too). Clients would
  need `CAP_NET_ADMIN`, and native publishers that want moq-congestion already
  have QUIC.
- ✅ Own XDP program and AF_XDP setup, with no C dependencies (rejected: aya,
  which needs bpf-linker and a nightly target for about 30 instructions;
  xsk-rs, which pulls in libbpf and libxdp). The program is BPF bytecode
  encoded in Rust and loaded with `bpf()`; UMEM and rings go through libc.
- ✅ Tokio first (rejected: the moq-uring workers first, which would wait on
  [their TCP serving](/quest/m4/stream-transports/ring.md)). This is a `moq-sock`
  primitive with a `moq-tokio` backend, one XSK per RX queue. The workers adopt
  it later, as they did the UDP demux.
- ✅ Opt in per listener, and fail loudly (rejected: try XDP and fall back to kernel TCP,
  which could hide a missing moq-congestion for weeks). The relay config names the
  interface; startup errors if the program or sockets cannot attach. Kernel TCP
  stays the default.

Decided 2026-11-19 by the agent, on the maintainer's "you choose the rest":

- The [comparison](/quest/m5/userspace-tcp/measure.md) runs a test binary on the
  stack before [the relay wiring](/quest/m5/userspace-tcp/relay.md) starts, so a
  no-win stops this epic before any public config ships.
- m4: no named consumer waits on it yet; it adds relay config, so it isn't m9.
  It moves to m3 once a deployment waits on it.
- Linux only, the only platform with AF_XDP. The privileged tests (XDP on a veth
  in a netns) run under sudo on a GitHub-hosted Linux runner and skip loudly elsewhere.
