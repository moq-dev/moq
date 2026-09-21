# [M] Unreal prototype: a UE5 module on the C++ package

## Goal

A written go/no-go verdict on shipping an Unreal Engine plugin. The evidence
is `cpp/unreal`: a UE5 module that links the prebuilt C++ package with
exceptions disabled, subscribes to a broadcast from a Blueprint node, and
renders decoded video into a `UTexture2D`, measured for frame latency and
editor stability across a play-stop-play cycle.

## Plan

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
- Verdict promotes a real `cpp/unreal` plugin quest into next or abandons this
  with the reasons.

## Required

- [Package](/quest/next/cpp/package.md) - the tarball the module links
- [Decoded frame ownership](/quest/next/decoded-frames.md) - the decoded frames the texture needs
