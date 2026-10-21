# [L] A runtime trait replaces web-async

## Goal

A MoQ runtime trait covers spawn and time (via `moq-clock`), with tokio,
browser, io_uring, and sim implementations, and replaces web-async in every
crate that uses it (moq-ffi, moq-mux, moq-net, moq-room, moq-stats, and
moq-wasm if it still exists), so a crate runs on any of them unchanged.

moq-relay spawns through it too, so its in-memory integration tests
(`rs/moq-relay/tests/integration/connection.rs`) move from paused tokio onto
the sim: today `Cluster::new` and the auth decider `tokio::spawn`.

## Plan

Decided 2026-10-10: deferred until [controlled time](/quest/m1/time/README.md)
lands; time came first because it is what tests need. Decide in a planning
pass whether I/O seams belong in the trait, given moq-quic's sans-IO core and
moq-sock already split I/O by runtime. The sim in `moq-clock` is its first
non-tokio implementation.

## Required

- [Controlled time](/quest/m1/time/README.md) - the time half this builds on
