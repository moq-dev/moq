# [M] C++ packages reuse the shared FFI build

## Goal

The C++ packages link the same default-feature moq-ffi libraries that the
shared FFI build produces for Go, Kotlin and Swift, instead of building their
own, once those libraries are proven compatible on all four C++ targets.
Installed C++ consumers pass with the prebuilt input. The separate `cpp-v*`
release lifecycle stays explicit: a C++ release still names which FFI build
it ships.

## Plan

The four-target comparison is recorded in [the FFI build evidence](/.github/ffi.md#c-comparison).
Generated bindings match on all four targets. The Linux x86 installed consumer
passes with the sampled shared static library; the other three consumer checks,
Rust LTO object differences, and remaining Windows object normalization are
still open. No incompatible ABI difference has been established.

Maintainer decision before implementation: recommend an explicit
`cpp/moq/FFI_VERSION` pin selecting immutable, checksummed default-feature native
assets on the corresponding `moq-ffi-v*` release. Those static assets are not
currently published for all four targets. Alternatives are a workflow-run
input with expiring artifacts, or retaining native C++ builds. Keep this quest
open until the input is settled and compatibility checks pass; the evidence
does not authorize a new publishing contract or build configuration.

Follow-up to [#5549](https://github.com/moq-dev/moq/pull/5549), which shares
the default FFI build across Go, Kotlin and Swift and records its evidence in
`.github/ffi.md`. The sampled C++ static libraries differed from the reuse
candidates, so C++ was left out.

- Prove compatibility before changing a build. Compare, per C++ target:
  target triple, features, profile, native versus cross host, glibc/CRT
  version, and UniFFI metadata. Record the comparison in `.github/ffi.md`
  beside the existing ones. A difference that matters ends the quest with
  the reason recorded; one that does not gets normalized, as #5549 did for
  Windows timestamps.
- Then design a prebuilt input for the CMake and build-script paths, and test
  installed consumers against it.
- Keep `cpp-v*` releases separate from `moq-ffi-v*`; the C++ release
  selects an FFI build explicitly rather than following the newest one.

## Related

- [C++ and OBS package proof](/quest/m1/cpp-package-check.md) - the installed-consumer checks this reuses
- [Nightly after FFI reuse](/quest/m0/ci/nightly-ffi-reuse.md) - confirms the shared build in production first
