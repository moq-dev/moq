# C++ through moq-ffi

## Goal

A C++ developer adds one registry line or one tarball, includes `<moq/moq.hpp>`,
and holds the whole moq-ffi surface (session, origin, broadcast, track, group,
media, audio and video) as RAII objects whose async operations return
cancellable futures, with no `user_data` plumbing, no handle integers, and no
thread of their own to babysit. The OBS plugin is the in-tree consumer that
proves the shape; external SDK users are the audience.

Non-goals: libmoq stays as the plain-C ABI, keeps its own release, and keeps
following the Cross-Package Sync table like every other wrapper; no
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

Errors never throw. The fork's `error_style = "expected"` flag makes every
fallible generated method return `uniffi::expected<T, moq::MoqError>` and every
future deliver the same, which is what lets Unreal and other `-fno-exceptions`
builds consume the package. `uniffi::expected` is `std::expected` on C++23 and a
bundled `tl::expected` below it; the `moq::` layer renames both. Callback
interfaces are refused under that flag until one is needed.

One library, C++17 floor (OBS's baseline), feature-gated extras: `co_await`
on a future under `__cpp_impl_coroutine`, `std::expected` under
`__cpp_lib_expected`. Never a second library per standard.

Distribution is all of: a release tarball with a CMake package config and
pkg-config file (mirroring `libmoq.yml`), a vcpkg registry we own, and a Conan
remote we own, the latter two fetching the prebuilt tarball so consumers never
need a Rust toolchain or the bindgen fork. vcpkg lands first; the Conan recipe
reads the same release manifest so a release bumps both.

## Quests

- [Package](/quest/m1/cpp/package.md) - the `cpp/moq` wrapper, CMake package, release tarball, interop client, and docs
- [Error messages](/quest/m1/cpp/error-message.md) - a C++ `moq::Error` prints the same message Rust gives
- [OBS migration](/quest/m1/cpp/obs.md) - the OBS plugin moves from libmoq handles and trampolines to the generated C++

## Related

- [C# through moq-ffi](/quest/m2/cs/README.md) - the same recipe with NordSecurity's C# generator
- [Unreal prototype](/quest/m2/unreal.md) - a UE5 module consumes the package with exceptions disabled
- [#2907](/quest/m4/2907-bind-the-browser-through-moq-ffi-uniffi-instead-of-a.md) - the browser reaches moq-ffi through a generator too; shares the Task-per-target findings
- [vcpkg registry](/quest/m2/cpp-vcpkg.md) - a registry we own serves the prebuilt package to `vcpkg` manifests
- [Conan remote](/quest/m2/cpp-conan.md) - a remote we own serves the same tarball to `conan install`
