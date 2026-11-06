# [M] C++ packages reuse the shared FFI build

## Goal

The C++ packages link the same default-feature moq-ffi libraries that the
shared FFI build produces for Go, Kotlin and Swift, instead of building their
own, once those libraries are proven compatible on all four C++ targets.
Installed C++ consumers pass with the prebuilt input. The separate `cpp-v*`
release lifecycle stays explicit: a C++ release still names which FFI build
it ships.

## Plan

The maintainer approved an explicit `cpp/moq/FFI_VERSION` pin selecting
immutable default-feature native assets on the corresponding `moq-ffi-v*`
release. `cpp/moq/FFI_SHA256.json` pins all four archives by complete filename
and SHA-256. Keep `cpp-v*` and `moq-ffi-v*` separate; never select the newest
FFI release or use expiring Actions artifacts as a release input.

The prebuilt CMake/build-script paths and publication gates are implemented.
Nightly packages reuse each target's checked same-run archive after its
installed consumers pass, without rebuilding moq-ffi. Direct CMake builds and
`just cpp check` use the current source checkout unless an explicit native
directory is supplied. Package-release builds select the pin when it is set;
a failed selected input never falls back to source.

The [four-host dry-run](https://github.com/moq-dev/moq/actions/runs/38969347067)
passed on implementation commit `cbbcf9b2c92aa88c5550d9175adcc317f8137f4a`.
Linux x86, Linux ARM, macOS ARM and Windows MSVC passed installed C++17 and
C++23 consumers, including both coroutine probes. Unix also passed pkg-config
and doc samples. All four jobs then verified/extracted their checked archives
and produced C++ packages. Later commits change only documentation and the
tag-only release-creation notes, which that dry-run does not exercise.

The [FFI build evidence](/.github/ffi.md#c-comparison) separates historical
archive/metadata comparisons from this current executable consumer proof.
Historical Windows assembly and Rust LTO comparisons do not prove complete
byte-for-byte code equivalence. The current four-host consumers have passed;
no older Linux runtime baseline is claimed. If source,
generator or configuration changes before activation, rerun the same four-host
gates on the final checkout.

Keep this quest open for the published release input:

- A real `moq-ffi-v*` release must publish all four default-feature native
  archives and their checksums after its installed-consumer gates pass. No
  existing release has this complete asset contract; no release or tag was
  created during this work.
- Propose that release's exact version and four archive hashes, then validate
  the actual downloaded release inputs through all four installed-consumer
  paths before landing the pin. Same-run artifacts and metadata comparisons
  alone do not satisfy this release-pin activation condition.
- Until those conditions hold, leave `FFI_VERSION` unset. Nightly may use its
  validated same-run inputs; standalone package builds retain source builds.
  Missing, malformed, incompatible or checksum-mismatched selected inputs fail.

Follow-up to [#5549](https://github.com/moq-dev/moq/pull/5549), which shared
the default FFI build across Go, Kotlin and Swift.
