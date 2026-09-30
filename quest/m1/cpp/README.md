# C++ through moq-ffi

## Goal

A C++ developer adds one registry line or one tarball, includes `<moq/moq.hpp>`,
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

Distribution is all of: a release tarball with a CMake package config and
pkg-config file (mirroring `libmoq.yml`), a vcpkg registry we own, and a Conan
remote we own, the latter two fetching the prebuilt tarball so consumers never
need a Rust toolchain or the bindgen fork. vcpkg lands first; the Conan recipe
reads the same release manifest so a release bumps both.

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

## Required

- [Generator](/quest/m1/cpp/generator.md) - the uniffi 0.32 C++ generator with futures and expected-style errors, pinned and generating `cpp/ffi` in CI
- [Package](/quest/m1/cpp/package.md) - the `cpp/moq` wrapper, CMake package, release tarball, interop client, and docs
- [Cancel](/quest/m1/cpp/cancel.md) - a cancelled or consumed future reports `valid() == false`, like `std::future`, and a read of it aborts with a message naming the misuse
- [macOS alias check](/quest/m1/cpp/macos-alias-check.md) - `just cpp check` passes its alias step with the BSD `sed` macOS ships
- [C++ standard](/quest/m1/cpp/cxx-standard.md) - a consumer that sets C++23 only on its own target links the package
- [OBS migration](/quest/m1/cpp/obs.md) - the OBS plugin moves from libmoq handles and trampolines to the generated C++

## Related

- [vcpkg registry](/quest/m2/cpp-vcpkg.md) - a registry we own serves the prebuilt package to `vcpkg` manifests
