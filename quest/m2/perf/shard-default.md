# [M] Shard the relay by default

## Goal

A saturating benchmark shows whether thread-per-core QUIC beats the shared
runtime, and the relay's default follows the measurement. If sharding wins,
an unset `runtime.workers` runs one worker per core on Linux, and
`workers = 0` is the explicit way back to the shared runtime. Off Linux, the
shared runtime stays the default.

## Plan

Sharding is opt-in: `runtime.workers` is unset by default, and
`runtime.pin` defaults off. The maintainer decided on #5688 to keep it that
way until a measurement justifies the flip.

- Extend `just bench-shard` to load that saturates the shared runtime, for
  example 1024+ connections or more than one load host. At 16 to 256
  connections, #5688's sweep never used one full core, so it showed no
  regression from sharding and no win either. The 16-shard groups spent about
  15 to 25% more CPU than the shared runtime at low load, from idle per-core
  runtimes.
- Flip the default only if sharding raises the throughput ceiling or cuts
  CPU per connection at saturation. A measured no-win deletes this quest.
- Include physical-core, SMT and NIC-local placement on multi-NUMA hardware
  against current pinning and an unpinned control. CPU selection currently
  walks allowed logical CPU IDs; it does not coordinate NIC RSS queues or IRQ
  affinity. Workers already pin before ring/pool allocation, so do not assume
  all pool memory starts on the main thread's node. Sweep workers × flows and
  publishers × subscribers, with quotas and multiple relay processes. Record
  per-core softirq time, migrations, remote memory/cache misses, CPU per useful
  byte, memory per worker and tail latency. Keep CID ownership intact while
  evaluating RSS/RPS/XPS alignment; the Linux
  [network scaling guide](https://docs.kernel.org/networking/scaling.html)
  explains their separate roles. Hardware-specific results must not silently
  become a universal pinning default.
- Shard only where a group serves the same listener the shared runtime would
  open. A stream-only relay, or one with a QUIC-LB `lb_id`, stays shared
  unless a count is set. A count sized from `available_parallelism`, capped at
  `moq_sock::shard::MAX_SHARDS`, follows cgroup quotas.
- With the default flipped, `runtime.io_uring` no longer needs an explicit
  count.
- Worker mode must deliver at least what the shared runtime does. A
  2026-10-22 runtime matrix (`f57c0aad`, one jemalloc + io-uring build,
  `bench/relay.sh`, 1 publisher x 256 subscribers at 60 fps of 1,200-byte
  one-frame groups, three rotated 60 s rounds on a 4-vCPU KVM host) measured:

  | runtime | median relay cores | median RSS MB | lifetime p99 ms | skipped groups per round |
  | --- | --- | --- | --- | --- |
  | shared tokio | 1.315 | 176.4 | 15-17 | 0 / 0 / 0 |
  | 2 tokio workers | 1.249 | 171.8 | 18-32 | 12 / 48 / 0 |
  | 2 io_uring workers | 0.946 | 187.2 | 17-18 | 0 / 0 / 0 |

  The generator shared the host (about 1.5 cores), so the tokio-worker skips
  may be CPU contention, connection imbalance across two workers, or a worker
  stall. Before flipping the default, reproduce them with the generator on a
  separate host and find the cause; the flip requires worker mode to skip no
  more groups than the shared runtime at the same load.

Update `doc/bin/relay/` for the default change.

## Related

- [kio channel contention](/quest/m2/perf/kio-channel-contention/README.md) - its per-thread spikes run on the workers
- [UDP demux](/quest/m2/p2p/udp-demux.md) - demuxes each shard's socket
- [Performance survey](/quest/m2/performance-survey/README.md) - paired benchmark evidence and memory/settings controls
- [Socket queues](/quest/m2/performance-survey/socket-queues.md) - queue sizing and local drop attribution
