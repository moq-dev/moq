# [XS] Turn off axum's tower-log

## Goal

moq's workspace builds axum without its default `tower-log` feature, so tracing's
`log` feature stops expanding into every crate. Record the instruction counts of a
moq-net edit and a clean build before and after.

## Plan

Decided with the maintainer on 2026-10-25, from kixelated/rust2's
[macro cost report](https://github.com/kixelated/rust2/pull/64): tracing's
`macro_rules!` output is half of what the four crates rebuilt after a moq-net edit
expand, mostly from its `log` feature, which `tower-log` turns on build-wide. Turning
it off cut 7.3% of the edit's instructions and 2.2% of a clean build's. The `log`
feature only forwards tracing events to a `log` logger when no tracing subscriber is
set, so check that no binary or binding installs a `log` logger instead of a tracing
subscriber.

Public API: none.
