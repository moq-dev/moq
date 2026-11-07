# [L] FFI handle state

## Goal

Each moq-ffi and moq-c handle holds its lifecycle in one value under one lock:

- A video producer's track, its bandwidth reservation, and its follower
  change together. A finish or abort can't leave a reservation behind.
- A one-shot request handle (track, broadcast, datagram, socket, and server
  requests) is pending, answered, or failed. A failed answer leaves the
  request pending, or reports why it failed.
- A background driver that never ran reports `Cancelled`, never `Ok`.
  Synchronous calls after `moq_ffi_shutdown` fail the same way the
  asynchronous ones do.

The language wrappers stop mirroring Rust state in their own flags.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

Bugs to reproduce and fix:

- **Video follow across locks** (`rs/moq-ffi/src/video.rs`):
  - `inner`, `reservation`, and `follow` are three mutexes.
  - `write()` releases `inner` and only then calls `follow_track`, so a
    `finish()` on another thread can run in between.
  - `follow_track` then stores a new reservation for a finished track.
  - `abort()` returns early once `inner` is `None` and never clears it.
  - Recommended: `Mutex<enum { Open { producer, follow: Option<(Reservation,
    Follow)> }, Closed }>`.
  - The same bug exists in `rs/moq-c/src/video.rs` (`follow_grant` spreads
    `reservation`, `follow`, and `followed` across three lock acquisitions).
    It also exists in the encoder `apply` of both crates: the encoder is taken
    before a fallible `discontinuity()`, so a failure leaves the catalog
    saying enabled while frames are dropped.
- **`Background` after shutdown** (`rs/moq-ffi/src/ffi.rs`; `rs/moq-c/src/ffi.rs`
  maps the same case to `Panic`):
  - `Option<Consumer<Option<Result>>>` encodes five states.
  - `check()` reads "dropped without a result" as `Ok`, while `finished()`
    reads it as `Cancel`.
  - Recommended: `enum Completion { Detached, Running, Done(Result), Dropped }`.

Also sweep:

- The `Mutex<Option<Request>>` handles: `producer.rs`, `origin.rs`,
  `datagram.rs`, `socket.rs`, `server.rs`, and `media.rs`'s `Requested`
  target. Each is taken before validation or before an await, and they return
  inconsistent errors (`Closed` vs `AlreadyResponded`).
- `Task<T>` (`ffi.rs`): a cancel flag, an `Option` state, and the cleanup.
- `MoqServer` (`server.rs`): its lifecycle is spread over four places.
- `MoqTrackProducer.keep: None`, which means both "finished" and "has no
  family".
- moq-c's global id counter, whose wrap lets a stale route id name a newer
  broadcast. Use generation-tagged ids or clear ids on close.

Then sweep the hand-written wrappers:

- the Python `Client`, which allows nested `async with`;
- Kotlin flows, which cancel only on `CancellationException`;
- the OBS output's mirrored `connected`/`live`;
- the Dart stream.

[moq-dev/moq#8060](https://github.com/moq-dev/moq/pull/8060) (moq-c parity)
edits `rs/moq-c/src/video.rs` and `ffi.rs`, so expect merge conflicts.
