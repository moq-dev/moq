# [M] FFI handle state

## Goal

The moq-ffi and moq-c handles that still spread one lifecycle over several
fields hold it in one value under one lock, and the language wrappers stop
mirroring Rust state in their own flags.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

[moq-dev/moq#8264](https://github.com/moq-dev/moq/pull/8264) fixed the
reproduced bugs: the video producer's track, reservation, and follower
under one lock in both crates; the encoder disable that left the catalog
enabled; `Background` reading a dropped driver as `Cancelled` everywhere;
and the one-shot request handles (`OneShot` in `rs/moq-ffi/src/ffi.rs`),
which now answer once and report `AlreadyResponded` or the first answer's
error. What remains has no known bug, so it needs no test beyond what each
redesign changes.

Sweep in moq-ffi and moq-c:

- `Task<T>` (`rs/moq-ffi/src/ffi.rs`): a cancel flag, an `Option` state, and
  the cleanup.
- `MoqServer` (`rs/moq-ffi/src/server.rs`): its lifecycle is spread over
  four places (`task`, `driver`, `listening`, and the task's state).
- `MoqTrackProducer.keep: None`, which means both "finished" and "has no
  family".
- moq-c's global id counter (`rs/moq-c/src/id.rs`), whose wrap lets a stale
  route id name a newer broadcast. Use generation-tagged ids or clear ids on
  close.

Then sweep the hand-written wrappers:

- the Python `Client`, which allows nested `async with`;
- Kotlin flows, which cancel only on `CancellationException`;
- the OBS output's mirrored `connected`/`live`;
- the Dart stream.

`MoqConnectionState` split into its own m2 quest, since it breaks every binding.
