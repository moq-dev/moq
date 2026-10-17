# [M] Richer simulation workloads

## Goal

The seeded simulation's random workloads in `rs/moq-quic/src/tests/sim.rs`
also exercise the paths the QUIC rewrites cut, each with its own invariant,
so a refactor that breaks one fails a replayable seed:

- **Resets**: RESET_STREAM and RESET_STREAM_AT mid-transfer. The reader
  sees an unmodified prefix of what was written, then the reset code. The
  prefix covers at least the reliable size for RESET_STREAM_AT and may be
  any length for RESET_STREAM, since bytes already read stay read. Stream
  and connection credit come back.
- **Stops**: STOP_SENDING mid-transfer. The sender observes the code
  through `SendStream::stopped` or `WriteError::Stopped`, the stopped
  reader sees nothing further, and credit comes back.
- **Bidirectional streams**: both sides write on the same streams, and every
  byte arrives once and in order in each direction.
- **Datagrams** interleaved with streams: each arrives at most once and
  unmodified, and datagrams never stall stream progress. On a lossless link
  with buffers sized for the workload, every datagram arrives, so dropping
  them all fails.
- **Migration**: NAT rebinding and active migration mid-workload. The
  connection survives and every delivery invariant still holds.

## Plan

Decided 2026-10-16 with the maintainer. Reuse the workload generator and
invariant checks from the seeded simulation, under the same PR budget (64
fixed cases) and nightly sweep (`just rs quic-sim`). If migration needs
address changes that `Pair`'s `Link`s cannot express, add the smallest
harness hook for it here; flag it on the PR if that pushes past [M].
