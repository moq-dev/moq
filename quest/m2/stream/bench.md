# [S] Stream benchmark

## Goal

A reproducible benchmark reports throughput and latency of a byte stream
through a two-relay chain against raw TCP over the same emulated path, with
and without loss on one hop, so the per-hop QoS claim is measured rather than
asserted.

## Plan

Reuse the existing bench harness and network shaper; record the numbers in the
quest's PR and on the bench site if it exists by then. A regression is not a
gate here; the result informs the docs and the mesh guide.

## Required

- [Rust byte streams](/quest/m2/stream/rust.md) - the stream under test
