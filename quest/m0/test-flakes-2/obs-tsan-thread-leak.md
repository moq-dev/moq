# [S] OBS TSan runs report no leaked FFI worker thread

## Goal

The OBS TSan unit tests (`Nightly (OBS TSan)` in
`.github/workflows/nightly.yml`) stop reporting a `thread leak` of a finished
`moq-ffi-worker` thread at exit on a loaded host, fixed at the cause under the
[epic](/quest/m0/test-flakes-2/README.md)'s rules.

## Plan

Added 2026-11-06 (maintainer, follow-up planning in `/quest-complete`), from
[moq-dev/moq#7996](https://github.com/moq-dev/moq/pull/7996), whose agent
saw the report on a loaded host while fixing the stop-drain test.

`moq-ffi-worker` is the tokio runtime's worker thread in
`rs/moq-ffi/src/ffi.rs`. Lead, unverified: TSan flags a thread that finished
but was never joined, so the runtime's workers may outlive the last FFI call
without a join at shutdown. Check whether the test exits before the runtime
shuts down, or whether moq-ffi's shutdown detaches its workers, before choosing
between joining them on shutdown and changing the test's teardown.

Reproduce first: run the OBS TSan tests repeatedly beside `just check --all`
load and keep the failing output.

Public API: none. Wire: none.
