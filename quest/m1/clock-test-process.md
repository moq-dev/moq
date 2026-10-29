# [XS] kio's clock tests pass under cargo test

## Goal

`cargo test -p kio --all-features` passes, not only under nextest.
`paused_runtimes_own_their_anchors` checks the same behavior without
depending on being the first test in its process to read the clock.

## Plan

Decided 2026-10-29 (maintainer), a follow-up of
[moq-dev/moq#7161](https://github.com/moq-dev/moq/pull/7161), whose agent hit
it. The test (`rs/kio/src/clock/runtime.rs`, from
[moq-dev/moq#5681](https://github.com/moq-dev/moq/pull/5681)) builds its two
paused runtimes before anything reads the clock, "so in a fresh process each
counts from its own origin". `cargo test` runs a crate's tests in one
process, so another test usually takes the origin first and the assertion fails,
every run. CI runs nextest, one process per test, so it
passes there and the failure only bites local `cargo test` runs.

Make the test independent of process state, for example by giving it a
fresh origin through the module's own test hooks or by running its body in a
child process, rather than marking it nextest-only. Keep what it proves: a
paused runtime counting from its own origin anchors on its own clock.

Public API: none. Wire: none.
