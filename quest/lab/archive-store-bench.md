# [S] Archive reads count their store round trips

## Goal

A test sweeps `moq_archive::Consumer` over track count and objects per track
and asserts how many LIST and GET calls each refresh and read makes, so a
change that adds store round trips to archive reads fails.

## Plan

- The cost is store round trips (per refresh, one LIST of the archive prefix
  plus one per track; one GET per object), which an `InMemory` Criterion
  bench would hide ([moq-dev/moq#6262](https://github.com/moq-dev/moq/pull/6262)).
- Count them with the crate's test store, `rs/moq-archive/src/mock.rs`, which
  already logs every GET, PUT, DELETE, and LIST (`Mock::take`), as
  `consumer/tests.rs` does for one case. Deterministic, so it needs no real
  object store, credentials, or Nightly job.
- Sweep both axes, track count and objects per track, per the fan-out
  benchmark rule.

Rewritten 2026-11-12 (/quest-audit): the probe against a real S3, R2, or
MinIO store is dropped. Its latency is the store's, and the round-trip count
it was after is measured exactly in-process.

Public API: none. Wire: none.
