# [M] Richer simulation workloads

## Goal

The seeded simulation's random workloads in `rs/moq-quic/src/tests/sim.rs`
also exercise the paths the QUIC rewrites cut, each with its own invariant,
so a refactor that breaks one fails a replayable seed:

- **Resets and stops**: RESET_STREAM, STOP_SENDING, and RESET_STREAM_AT
  mid-transfer. The reader gets exactly the bytes up to the reliable size,
  then the reset code, and stream and connection credit come back.
- **Bidirectional streams**: both sides write on the same streams, and every
  byte arrives once and in order in each direction.
- **Datagrams** interleaved with streams: each arrives at most once and
  unmodified, and datagrams never stall stream progress.
- **Migration**: NAT rebinding and active migration mid-workload. The
  connection survives and every delivery invariant still holds.

## Plan

Decided 2026-10-16 with the maintainer. Reuse the workload generator and
invariant checks from the seeded simulation, under the same PR budget (64
fixed cases) and nightly sweep (`just rs quic-sim`). If migration needs
address changes that `Pair`'s `Link`s cannot express, add the smallest
harness hook for it here; flag it on the PR if that pushes past [M].

## Required

- [Seeded simulation](/quest/m1/quic/coverage/sim.md) - the impaired, deterministic `Pair` and the property tests this extends
