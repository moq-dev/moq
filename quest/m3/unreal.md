# [M] Unreal prototype: a UE5 module on the C++ package

## Goal

A written go/no-go verdict on shipping an Unreal Engine plugin. The evidence
is `cpp/unreal`: a UE5 module that links the prebuilt C++ package with
exceptions disabled, subscribes to a broadcast from a Blueprint node, and
renders decoded video into a `UTexture2D`, measured for frame latency and
editor stability across a play-stop-play cycle.

## Plan

In m3 until an Unreal consumer asks; the C++ package it links already exists.

- Consume the release tarball through the module's `Build.cs`
  (`PublicAdditionalLibraries`, include paths); Unreal's build does not use
  vcpkg or CMake, which is why the tarball exists alongside the registries.
  Keep `bEnableExceptions` off to prove the `expected` error style holds.
- Marshal futures onto the game thread with `AsyncTask(ENamedThreads::GameThread, ...)`
  as the `moq::Executor`; never touch `UObject`s from a continuation.
- Decode: the moq-ffi video consumer feeds the texture; the CPU frame path
  is enough for the verdict, GPU import is not measured here.
- Record: editor hot-reload behavior with a static Rust library loaded, the
  dispatcher shutdown on module unload, and package size.
- A positive verdict proposes a plugin quest in the then-current feature
  horizon for maintainer prioritization; it does not automatically gate a
  release. Otherwise abandon this with the reasons.

## Required

- [First C++ package release](/quest/m2/release/cpp-release.md) - the tarball the module links
