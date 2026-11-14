# [S] Nightly's release builds pass

## Goal

The release builds Nightly runs pass every night, so the jobs that need them,
such as `Shipping sizes`, run instead of being skipped.

## Plan

Found by the triage of Nightly run
[43447849289](https://github.com/moq-dev/moq/actions/runs/43447849289)
(2026-11-13): `Shipping sizes` was skipped 11-08 to 11-12 because release
builds it depends on failed, so six days of size growth surfaced at once.
Failures seen, each with its own cause:

- 11-12, `moq-cli / release / Build (armv7-unknown-linux-gnueabihf)`:
  `error[E0463]: can't find crate for core`, so the toolchain lacks that
  target's standard library.
- 11-11, `Release FFI / Shared moq-ffi / Build moq-ffi (aarch64-unknown-linux-gnu)`:
  apt failed with `Temporary failure resolving 'security.ubuntu.com'`, and
  `Release C`/`Release Cpp` aarch64 then failed on a missing
  `cpp-native-aarch64-unknown-linux-gnu` artifact.
- 11-11/11-12: the macOS `moq-cli` tarball, the Go FFI bindings and package,
  the Kotlin FFI package, and the aarch64 `moq-gst` builds, not yet triaged.

Fix each at its root. A network fetch at build time that can fail on a DNS
blip is a defect to remove, for example by taking the cross toolchain from
Nix, not to retry.

Public API: none. Wire: none.
