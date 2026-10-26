# [XS] Turn off axum's tower-log

## Goal

moq's workspace builds axum without its default `tower-log` feature, so tracing's
`log` feature stops expanding into every crate. Record the moq-net edit and clean
build's instructions or CPU time before and after.

## Plan

Decided with the maintainer on 2026-10-25, from kixelated/rust2's
[macro cost report](https://github.com/kixelated/rust2/pull/64): tracing's
`macro_rules!` output is half of what the four crates rebuilt after a moq-net edit
expand, mostly from its `log` feature, which `tower-log` turns on build-wide. Turning
it off cut 7.3% of the edit's instructions and 2.2% of a clean build's. Check that no
crate relies on tracing events reaching the `log` facade, such as a dependency
logging through `log` while moq installs only a tracing subscriber.

Public API: none.
