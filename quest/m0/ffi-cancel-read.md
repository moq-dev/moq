# [M] A cancelled moq-ffi read never takes data

## Goal

Cancelling a moq-ffi async read (`read_frame`, `next`, `next_group`, and the
other consumer reads) in any binding leaves the data it was waiting for to
the next read, as the method docs promise. Today the cancelled read can take
it, and the next read hangs: `cpp/moq/test/probe.cpp` fails at its
checks that read after a cancelled read in about 1 of 30 runs at C++17
and 2 of 20 at C++23 locally, which flakes the `cpp.yml` check.

## Plan

Found while iterating on #4079. `Task::run` and `detached` in
`rs/moq-ffi/src/ffi.rs` spawn the work as a tokio task tied to the caller's
future with `AbortOnDrop`. A foreign `cancel()` reaches uniffi's
`rust_future_cancel`, which only wakes the continuation. The Rust future,
and with it the `AbortOnDrop`, drops at `rust_future_free`, which the
bindings call later from their executor. Until then the spawned closure
still holds the consumer's lock, so it takes the next frame, which goes
nowhere, while the next read waits behind it.

Both known fixes stop the work when the cancel happens, not when the handle
is freed:

- Abort the spawned task when the uniffi future is cancelled, not only when
  it drops.
- Await reads in place instead of spawning them, so a cancelled future that
  is never polled again cannot make progress. Reads that need a tokio
  context keep the spawn.

Land it with a Rust regression test in moq-ffi that cancels a pending read
without freeing it, writes a frame, and requires the next read to return
it. The C++ probe already covers it end to end.
