# [M] uniffi-bindgen-cs on uniffi 0.32

## Goal

`just cs check` regenerates `cs/ffi` from `rs/moq-ffi` with a pinned C#
generator and runs a small xunit program that connects, awaits a subscribed
frame as a `Task`, cancels a pending read with a `CancellationToken`, and
catches a typed `MoqException`, on the host runtime.

## Plan

- Fork `NordSecurity/uniffi-bindgen-cs` at `v0.11.0+v0.31.0` into
  `kixelated/uniffi-bindgen-cs` and port it to uniffi 0.32.x, following the
  Go port. Tag `vX.Y.Z+v0.32.0`; offer the port upstream and drop the fork
  when NordSecurity tags.
- Pin it in `flake.nix` beside the other generators with the same list of
  places that name the version. `just cs check` regenerates into `cs/ffi`
  and runs the probe; wire it into the check workflow like `just go check`.
- Check how the generator maps moq-ffi's cancel-on-drop tasks onto
  `CancellationToken` and `IDisposable`; record any gap in the package quest
  rather than patching it in a wrapper.

## Related

- [C++ generator](/quest/m2/cpp/generator.md) - the same port against the C++ generator
