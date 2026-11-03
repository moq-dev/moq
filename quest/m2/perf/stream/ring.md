# [L] TCP streams on the worker

## Goal

`moq-uring` serves TCP from the same worker that serves UDP, and hyper (and
therefore axum) runs on top of it unchanged.

## Plan

A `tcp` module beside `udp`, in `moq-uring` rather than a new crate: it needs
the ring, the buffer pools, the timer heap, and the local task set that the
worker already owns, and splitting those across crates would mean exporting
ring internals for one consumer. Accept, connect, read, and write, using
whatever the ablation showed actually pays (multishot accept and receive,
provided buffers, registered fixed files), with the same borrow-the-pool
receive backpressure the UDP path uses.

Then `hyper::rt::{Read, Write, Executor}` implementations over those streams,
and `tokio::io::{AsyncRead, AsyncWrite}` for qmux's `transport::Stream`, which
its `Driver` polls. hyper is runtime-agnostic by
design, so axum's routers, extractors, and CORS keep working. The relay's
WebSocket upgrade reclaims the ring stream through `Upgraded::downcast` and
hands it to a qmux `Driver` spawned on the worker's local task set, so a
connection never leaves the thread that accepted it.

hyper is already a workspace dependency: the relay reclaims the socket under
a WebSocket upgrade through `hyper::upgrade` (`rs/moq-relay/src/websocket.rs`).

Cover it the way the UDP path is covered: tests that run against whichever
backend is compiled, an HTTP round trip through hyper on the worker, and a
WebSocket upgrade. Everything skips loudly below the kernel floor, as the
existing suite does.

## Required

- [Ring TCP ablation](/quest/m2/perf/stream/ablation.md) - picks the mechanisms before the worker implementation starts
