# [XS] macOS CI runs kio's tests

## Goal

The macOS test job runs `kio`'s tests, so platform-dependent kio failures,
such as the `Mutex` size assumption
[moq-dev/moq#7914](https://github.com/moq-dev/moq/pull/7914) fixed, fail CI
instead of reaching main.

## Plan

Decided 2026-11-05 (maintainer, follow-up from #7914): ✅ m0.

`just rs macos-test` (`rs/justfile`) tests hang, moq-tokio, moq-sock,
moq-video, and moq-auth, but not kio. Add it, and check the job's time stays
reasonable.
