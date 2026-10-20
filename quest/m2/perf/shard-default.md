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
- Shard only where a group serves the same listener the shared runtime would
  open. A stream-only relay, or one with a QUIC-LB `lb_id`, stays shared
  unless a count is set. A count sized from `available_parallelism`, capped at
  `moq_sock::shard::MAX_SHARDS`, follows cgroup quotas.
- With the default flipped, `runtime.io_uring` no longer needs an explicit
  count.

Update `doc/bin/relay/` for the default change.

## Related

- [kio channel contention](/quest/m2/perf/kio-channel-contention/README.md) - its per-thread spikes run on the workers
- [UDP demux](/quest/m2/p2p/udp-demux.md) - demuxes each shard's socket
