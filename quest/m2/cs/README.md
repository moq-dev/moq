# C# through moq-ffi

## Goal

A .NET developer adds the NuGet package and holds the moq-ffi surface as
classes whose async operations are `Task`s, with native libraries for every
runtime identifier in the package. Same recipe as the C++ line: generate from
moq-ffi with a pinned generator, add a thin idiomatic wrapper, never hand-roll
a mirror. Unity is a separate prototype.

## Plan

NordSecurity's `uniffi-bindgen-cs` (latest `v0.11.0+v0.31.0`) already emits
async methods as `Task<T>` and async callback interfaces; it needs the same
uniffi 0.32 port the Go, Dart, and C++ generators got. Plain .NET first; the
IL2CPP constraints Unity adds (static `MonoPInvokeCallback` trampolines, no
dynamic loading) are measured in the next prototype rather than designed around
up front.

## Quests

- [Generator](/quest/m2/cs/generator.md) - uniffi-bindgen-cs on uniffi 0.32, pinned and generating `cs/ffi` in CI
- [Package](/quest/m2/cs/package.md) - the `cs/moq` wrapper, NuGet package with native runtimes, smoke client, and docs

## Related

- [C++ through moq-ffi](/quest/m1/cpp/README.md) - the sibling line this copies
- [Unity prototype](/quest/m2/unity.md) - the package under IL2CPP
