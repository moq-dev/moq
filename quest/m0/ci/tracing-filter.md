# [XS] tracing-test stops forcing TRACE everywhere

## Goal

moq-tokio's tests no longer turn on `tracing-test`'s `no-env-filter` feature
for the whole build, so other crates' tests stop formatting every event at
TRACE, which costs 11 to 17% of the moq-tokio integration tests' CPU.

## Plan

Found by [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)'s
profile. Cargo feature unification enables the feature for every crate that
shares the build. Narrow it to the tests that need it, or filter per test,
and measure the integration tests before and after.

Public API: none.
