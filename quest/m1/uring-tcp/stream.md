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

Then `hyper::rt::{Read, Write, Executor}` implementations over those streams.
hyper is runtime-agnostic by design, so this is the whole adapter: axum's
routers, extractors, CORS, and its WebSocket upgrade keep working, and
`qmux`'s `ws` feature stays intact. The `Executor` spawns onto the worker's
local task set, so a connection never leaves the thread that accepted it.

Cover it the way the UDP path is covered: tests that run against whichever
backend is compiled, an HTTP round trip through hyper on the worker, and a
WebSocket upgrade. Everything skips loudly below the kernel floor, as the
existing suite does.
