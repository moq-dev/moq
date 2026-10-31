# [XS] moq-net's library tests pass under cargo test

## Goal

`cargo test -p moq-net --lib` passes, not only under nextest:
`setup::tests::credentials_stay_out_of_traces` and the model tests using
`model/test_tracing.rs` no longer each install a process-global tracing
subscriber, so whichever runs second in the shared process still captures
its events.

## Plan

Decided 2026-10-31 (maintainer, the /quest-complete requeue session).
Nextest runs each test in its own process, which hides the clash.

`credentials_stay_out_of_traces` (`rs/moq-net/src/setup.rs:311`) uses
`#[tracing_test::traced_test]`, and `count_drop_warnings`
(`rs/moq-net/src/model/test_tracing.rs:39`) calls
`tracing::subscriber::set_global_default` and expects no other global
subscriber. Its doc explains why it is global: tracing caches callsite
interest process-wide, so a scoped dispatcher misses events another thread
cached as disabled. Keep that property. Likely one shared global test
subscriber that both use (`count_drop_warnings`' thread-local capture slot
extended to the credential check), or the credential test moved onto
`count_drop_warnings`' capture. Confirm with `cargo test -p moq-net --lib`.

Public API: none. Wire: none.

## Related

- [moq-clock's tests pass under cargo test](/quest/m1/clock-test-process.md) - the same nextest-only pass in moq-clock
