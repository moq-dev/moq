# [M] Kernel pacing

## Goal

A measured verdict on handing packet pacing to the kernel. Today noq's pacer
is a userspace token bucket, and the io_uring driver ignores its hint
entirely: a GSO train leaves as one burst, bounded only by the congestion
window. Either the kernel paces each train (`SO_TXTIME` with the `etf` or
`fq` qdisc, per-packet transmit times in the cmsg the driver already builds)
and burstiness at the bottleneck drops without costing CPU, or the burst is
shown not to matter on the fleet's paths.

## Plan

- Measure first: on a netem bottleneck, compare inter-packet gaps and queue
  occupancy for a 64-segment GSO train against the same bytes paced at the
  controller's rate. If the bottleneck absorbs the burst without loss or
  delay at the fleet's typical rates, record it and stop.
- Then the io_uring path: stamp each GSO train with `SCM_TXTIME` at the time
  noq's pacer would have released it, one timestamp per train (the kernel
  spreads segments only with `fq`'s pacing, so test both qdiscs). The socket
  is shared across connections per worker, so per-socket rate limits
  (`SO_MAX_PACING_RATE`) cannot express per-connection pacing; only
  per-packet times can.
- Report CPU per Gbps, loss, and p99 latency against the userspace pacer on
  the tokio path and the unpaced io_uring path, on a real NIC as well as
  loopback, since `etf` needs hardware offload to be exact.

The verdict names which qdisc the relay hosts need, or that none is worth
configuring.

## Related

- [Send batching](/quest/future/quic-send-batching.md) - the other syscall-side
  lever on the same path
- [#3201](/quest/next/perf/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) -
  zero-copy on the same trains
