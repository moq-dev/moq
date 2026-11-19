# [L] A dropped group costs a subscription O(1), not O(ranges)

## Goal

Dropping a group from a subscription with many ranges costs about the same as with one, so moq-net's `publisher` bench `wake/8192` finishes in seconds instead of running past 600 s and timing out the Nightly `Rust bench targets` job (55-minute limit). A new bench sweeps drop cost over ranges × drops, so a regression shows in Nightly's measured coverage.

## Plan

Caused by [moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638): each dropped group makes the subscription rebuild state over every range, about 2.4 ms per drop at 8193 ranges. Four places rebuild instead of updating incrementally, in `rs/moq-net/src/model/track.rs` and `subscription/{window,runs}.rs`: the window, what is owed, the misses check, and the servable set. Make each incremental, and keep the JS counterpart in step where it has the same shape.

Decided 2026-11-18 (maintainer, /quest-plan): ✅ no stopgap (rejected: having the bench fixture do all its drops before one step, which takes 6 ms but hides the bug from the only bench that catches it). The Rust bench job stays red until this lands.

Public API: none. Wire: none.
