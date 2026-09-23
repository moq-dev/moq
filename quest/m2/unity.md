# [M] Unity prototype: the C# package under IL2CPP

## Goal

A written go/no-go verdict on a Unity package. The evidence is a sample
project that imports the NuGet package's managed assembly and native
libraries as a Unity plugin, builds with IL2CPP for desktop and one mobile
target, subscribes to a broadcast, and plays decoded audio through an
`AudioSource`, measured for startup and GC pressure.

## Plan

- IL2CPP forbids dynamic callback marshaling: every reverse P/Invoke needs a
  static method with `[MonoPInvokeCallback]`. Audit what the generated `cs/ffi`
  emits for callback interfaces and futures, and whether the generator needs
  an IL2CPP option or Unity needs a shim.
- Native libraries: `Plugins/<platform>` layout from the NuGet `runtimes/`
  tree, plus an Android `.so` and iOS static library the .NET package does not
  ship, which the verdict prices.
- Threading: continuations must hop to the main thread for any `UnityEngine`
  call; a `SynchronizationContext`-based executor is the likely answer.
- Verdict promotes a Unity package quest into m1 or abandons this with the
  reasons.

## Required

- [Package](/quest/m2/cs/package.md) - the NuGet whose contents Unity imports
