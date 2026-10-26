# [XS] Measure the tower-log build savings

## Goal

Record the instruction counts of a moq-net edit and a clean relay build before
and after disabling axum's `tower-log` feature.

## Plan

The workspace disables axum's `tower-log` feature while preserving its other
defaults. Relay, CLI, and binding feature graphs no longer enable `tracing/log`.
A full workspace build still enables it through the studio's calloop dependency.
Binary and binding logger initialization uses tracing subscribers.

Hardware counters are unavailable on the implementation host:
`perf stat -e instructions true` fails with `perf_event_paranoid = 4`, including
outside the sandbox. Run the comparison on a host with instruction counters.
Build `-p moq-relay`, so unrelated studio features do not mask the savings.

Decided with the maintainer on 2026-10-25, from kixelated/rust2's
[macro cost report](https://github.com/kixelated/rust2/pull/64): tracing's
`macro_rules!` output is half of what the four crates rebuilt after a moq-net edit
expand, mostly from its `log` feature, which `tower-log` turns on build-wide. Turning
it off cut 7.3% of the edit's instructions and 2.2% of a clean build's. The `log`
feature only forwards tracing events to a `log` logger when no tracing subscriber is
set.

Public API: none.
