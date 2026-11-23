# [S] moq-net's refresh-join coalescing test fails under a loaded suite

## Goal

`moq-net`'s `lite::subscriber::tests::refresh_joins_coalesce_upstream` passes every
full `cargo test -p moq-net --features async --lib` run, fixed at its cause.

## Plan

On `main` at d2a4ba4ace it failed 2 of 3 full lib runs on `fedora`, with
`joins coalesce into one request` seeing `[1, 1]` instead of `[1]`: two
SUBSCRIBE_UPDATEs asked for group 1 where the eight joins should coalesce
into one. It passed 40 isolated runs and on the failing run's
`MOQ_SIM_SEED`, so the order depends on something the sim clock does not
drive, such as a wall-clock read or a task woken by another test's load.

Reproduce under load first, such as the full lib suite or
`cargo nextest run -p moq-net --features async --lib refresh_joins_coalesce_upstream --stress-count 200`
beside a CPU-heavy job, then find what lets the ninth update through.
