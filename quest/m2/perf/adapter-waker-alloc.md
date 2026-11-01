# [S] Adapter reads reuse their waker

## Goal

A measured verdict on what a parking read costs in the production adapters
(moq-tokio, moq-uring) since
[moq-dev/moq#7042](https://github.com/moq-dev/moq/pull/7042), and a fix if
the cost is real: a stream read that has to wait stops allocating a fresh
shared waker each time.

## Plan

Decided 2026-11-01 (maintainer, `/quest-plan`): measure before fixing, since
no bench has shown the cost yet.

Since #7042, transport polls take a `kio::Waiter` through `&self`, and every
`kio::wait` future (`rs/kio/src/waiter.rs`) holds a waiter of its own. The
moq-net coding layer (`rs/moq-net/src/coding/reader.rs`, `writer.rs`) wraps
each decode or flush in one, so a read that parks allocates the waiter's
shared `Arc<Waker>` every time, where a per-stream `kio::Park` used to reuse
one.

[moq-dev/moq#7262](https://github.com/moq-dev/moq/pull/7262) fixed the
in-memory transport's version (it registered on the session close per read).
The memory-transport benches rarely park, so they cannot see the adapters'
cost.

Measure first: user-space instruction counts and allocations, pinned to one
core, on moq-uring's `session_lite` bench (`just rs bench-session`) and, for
moq-tokio, which has no crate-level session bench, the relay's
`rs/moq-relay/benches/session.rs` or `just bench BASE`. Measure a revision
before #7042 next to main. A measured no-win deletes this quest.

If it is real, let a stream keep one waiter (or `kio::Park`) across reads
again, for example in `coding::Reader`, instead of one per `kio::wait`
future.
