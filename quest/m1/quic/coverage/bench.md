# [M] Benchmark moq-quic

## Goal

`rs/moq-quic/benches/` holds Criterion benchmarks that each QUIC rewrite
compares against the code it replaced. They run in the nightly Criterion
smoke with the other crates.

## Plan

Decided 2026-10-16. Land this first in the epic, so the open rewrites can
compare against it.

`just bench <base>` only compares cases that exist at `<base>`, so the base
is the commit that lands this suite, not b30289127. Landed before a
rewrite, that commit is still pre-rewrite: the rewrite rebases and runs
`just bench <suite commit>`. For a rewrite that merges first, compare its
parent with this suite cherry-picked onto it against its merge commit with
the same suite, and record those numbers in this PR.

- Drive a sans-IO `Pair` (or a bench-local equivalent built from the public
  API) with no-op packet protection, ported from quinn `perf`'s
  `noprotection.rs`, so only protocol CPU is measured. Use only API present
  on the baseline and kept by the open rewrites, or the base comparison
  cannot build.
- Cases, each swept over both of its axes so a cost that grows with the
  table shows up as a slope:
  - **Stream send and scheduling**: stream count x priority levels, and
    bytes per write. This is what #5711 rewrites.
  - **Receive, reassembly, and reset**: out-of-order ingest across many
    partial streams, and reset and STOP_SENDING churn. Covers #5690 and
    #5735.
  - **Endpoint routing**: datagram dispatch over connections x CIDs per
    connection. Covers #5688.
  - **Wire codec and ACKs**: frame and packet encode and decode, and ACK
    processing over large range sets.
- The existing `#[ignore]` print-only benchmarks (`bbr3/cleanup_bench.rs`,
  `benchmark_receive_credit`) stay as they are.
