# [M] uniffi-bindgen-cpp: futures and expected-style errors on uniffi 0.32

## Goal

`just cpp check` regenerates `cpp/ffi` from `rs/moq-ffi` with a pinned C++
generator and compiles a small program that connects, subscribes to a track,
reads one frame through a future, cancels a pending read, and observes an
error as a returned `expected`, on gcc, clang, and MSVC at C++17. This is the
go/no-go gate for the questline: if the generator cannot be made to hold,
stop here and write down why.

## Plan

- Fork `livekit/uniffi-bindgen-cpp` at branch `livekit/uniffi-0.31-async`
  (futures with `get`/`wait_for`/`cancel`/`then`, async callback interfaces,
  a pluggable dispatcher; last commits 2026-09-08) into
  `kixelated/uniffi-bindgen-cpp`. Port it to uniffi 0.32.x: the metadata
  encoding changed in 0.32 without a contract bump, so a 0.31 generator fails
  to read a 0.32 cdylib at all; the Go port (`kixelated/uniffi-bindgen-go`
  `v0.8.0+v0.32.0`) is the worked example. Tag `vX.Y.Z+v0.32.0`.
- Add an `error_style = expected` generator option: methods return
  `moq::expected<T, Error>` instead of throwing, `uniffi::Future<T>::get()`
  returns the same, `then` continuations already receive a result type. Ship
  a bundled `tl::expected` and alias `std::expected` when `__cpp_lib_expected`
  is defined. Callback interfaces implemented in C++ return errors the same
  way. The exceptions style stays the default so the fork remains upstreamable.
- Verify the generated headers compile with `-fno-exceptions` under the
  expected style (Unreal's default), and that the async dispatcher shuts down
  cleanly when the consumer unloads (`uniffi::shutdown_async_dispatcher`).
- Pin the fork in `flake.nix` next to `uniffi-bindgen-go` and `uniffi-bindgen-dart`
  with the same comment discipline: every place that names the generator
  version is listed and bumped together (`rs/moq-ffi/build.sh`, the release
  workflow, `cpp/ffi/README.md`, `doc/lib/cpp`). `just cpp check` regenerates
  into `cpp/ffi` and compiles the probe; wire it into the check workflow the way
  `just go check` is.
- Document the cancellation contract from what moq-ffi does: native
  `Task::run` and `detached` hold an `AbortOnDrop` on the spawned task
  (`rs/moq-ffi/src/ffi.rs`), so dropping the Rust future aborts the work, and
  a mid-write abort is possible. The generated future's `cancel()` and its
  destructor map onto that; the wrapper says so where it matters (finish a
  group before dropping its write future).
- Offer both the 0.32 port and the expected flag upstream to LiveKit and
  NordSecurity; the fork exists only until they tag.

## Related

- [#2907](/quest/m4/2907-bind-the-browser-through-moq-ffi-uniffi-instead-of-a.md) - the browser generator spike; same Task and `#[cfg]`-inside-export gotchas apply
- [C# generator](/quest/m2/cs/generator.md) - the same 0.32 port against NordSecurity's C# generator
