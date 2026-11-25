# [S] Pull requests build moq-net's fuzz feature on its own

## Goal

A pull request that breaks `moq-net`'s `fuzz` feature fails the PR gate, instead of compiling green in the workspace build and breaking every Nightly fuzz job and `-p moq-mux` on macOS.

## Plan

Found by the Nightly triage of run [45459585582](https://github.com/moq-dev/moq/actions/runs/45459585582): [moq-dev/moq#9467](https://github.com/moq-dev/moq/pull/9467) put `origin::Config::new` behind `cfg(any(test, feature = "test-support"))`, but `rs/moq-net/src/fuzz/session.rs` still called it. The workspace build gets `test-support` from another crate, so only builds that enable `fuzz` alone broke. It is the second recent fuzz-build break ([moq-dev/moq#9505](https://github.com/moq-dev/moq/pull/9505), [moq-dev/moq#9555](https://github.com/moq-dev/moq/pull/9555)).

Decided 2026-11-25 (maintainer, /quest-plan): ✅ a PR-gate step equivalent to `cargo check -p moq-net --features fuzz --lib` (rejected: checking every test-only crate alone, which costs more gate time for breaks not yet seen; drop, relying on the Nightly). Run it only when the diff selects `moq-net` or its dependencies, and state its measured gate cost in the PR.
