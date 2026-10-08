# [S] C++ through moq-ffi

## Goal

A C++ developer adds one release tarball, includes `<moq/moq.hpp>`,
and holds the whole moq-ffi surface (session, origin, broadcast, track, group,
media, audio and video) as RAII objects whose async operations return
cancellable futures, with no `user_data` plumbing, no handle integers, and no
thread of their own to babysit. The OBS plugin is the in-tree consumer that
proves the shape; external SDK users are the audience.

Non-goals: the plain-C ABI, which moves to C generated from moq-ffi in
[Generated C bindings](/quest/m1/c/README.md); no
second hand-written C++ surface (ergonomics are fixed in moq-ffi where every
binding benefits); no wire or public Rust API change.

## Plan

Generate, do not hand-roll. The generated half comes from a kixelated fork of
LiveKit's `uniffi-bindgen-cpp` async branch, ported to uniffi 0.32 the way the
Go and Dart generators were (`flake.nix` pins the fork; move back upstream when
a tagged release catches up). The hand-written half is a thin `moq::` layer,
like `py/moq-rs` and `dart/moq`: naming sugar, a coroutine awaiter, an
`expected` alias, and executor wiring, nothing that mirrors a method.

Async in C++ has no runtime, so the shape is a future you either block on or
attach a continuation to: `uniffi::Future<T>` with `get()`, `wait_for()`,
`cancel()`, and `std::move(f).then(executor, cb)`. Destroying an incomplete
future cancels the Rust future, which is the cleanup-in-Drop rule from the
Rust side carried across. Continuations run on a default bounded dispatcher
unless the application installs its own (`uniffi::set_async_dispatcher`); OBS
installs one that hops to its own threads.

Errors never throw. The fork gains an `error_style = expected` flag so every
generated method returns `moq::expected<T, moq::Error>` and futures deliver the
same, which is what lets Unreal and other `-fno-exceptions` builds consume the
package. `moq::expected` is `std::expected` on C++23 and a bundled
`tl::expected` below it.

One library, C++17 floor (OBS's baseline), feature-gated extras: `co_await`
on a future under `__cpp_impl_coroutine`, `std::expected` under
`__cpp_lib_expected`. Never a second library per standard.

Distribution is a release tarball with a CMake package config and pkg-config
file (mirroring `moq-c.yml`), so consumers never need a Rust toolchain or the
bindgen fork. Decided in the 2026-09-30 audit: this line promises the tarball
only. A [vcpkg registry](/quest/m3/cpp-vcpkg.md) and a
[Conan remote](/quest/m3/cpp-conan.md) fetch the same tarball later and stay
deferred.

Confirmed in [#4100](https://github.com/moq-dev/moq/pull/4100):

- The fork's base is LiveKit's PR #1 (`uniffi-0.31-async`), not PR #5,
  which runs a worker thread per in-flight future and has no `then()` or
  async callback interfaces, both of which OBS needs.
- Fork tags keep upstream's `v<generator>+v<uniffi>` scheme with a
  `-kixelated.N` pre-release (`v0.11.0-kixelated.1+v0.32.2`), matching the
  Dart fork, so they never collide with an upstream tag.
- Cancelling abandons the future rather than delivering an error: a generic
  `E` has no cancelled variant, and a `std::variant<E, Cancelled>` would
  burden every call site.
- Callback interfaces are refused under `error_style = "expected"` until a
  consumer needs one; their bridge is built on `std::exception_ptr`.
- MSVC is covered by the post-merge nightly, not a branch dispatch: branches
  never dispatch the nightly.

Decided in the 2026-10-05 audit: the [FFI shape](/quest/m1/ffi-shape/README.md)
line (#4519) lands first, and this line ports `cpp/moq`, `cpp/obs`, and the
C++ interop client onto the reshaped moq-ffi, since its branch still calls
APIs FFI shape deleted.

Recorded in the 2026-10-06 audit: every child (generator #4100, package
#4187, cancel, C++ standard, macOS alias check, OBS migration #4281) merged
into the line branch, which deletes their quest files, so main deletes them
too. What remains is landing [#4079](https://github.com/moq-dev/moq/pull/4079).

## Required

- [FFI shape](/quest/m1/ffi-shape/README.md) - lands first; this line ports onto its moq-ffi

## Related

- [vcpkg registry](/quest/m3/cpp-vcpkg.md) - a registry we own serves the prebuilt package to `vcpkg` manifests
- [Conan remote](/quest/m3/cpp-conan.md) - a remote we own serves the prebuilt package to Conan
- [Unreal prototype](/quest/m3/unreal.md) - a UE5 module consumes the package with exceptions disabled
