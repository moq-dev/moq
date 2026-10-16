# Shared FFI releases

`release-shared-ffi.yml` owns the existing `moq-ffi-v*` tag trigger for Go,
Kotlin and Swift. Nightly calls the same coordinator without publisher secrets.
It builds nine distinct default-feature targets, replacing fourteen native
builds across those packages. Dart retains its eleven minimal-feature targets;
Python retains its wheel builds and Python-specific environment.

C++ retains its host CMake builds and independent `cpp-v*` release tags. All
four sampled C++ static libraries have different hashes from the candidates
shared here. In particular, ARM Linux builds natively on Ubuntu 24.04 rather
than on the shared cross-compilation host. Sharing those packages needs
separate artifact/ABI proof and a settled prebuilt-library CMake interface.
The [C++ comparison](#c-comparison) below narrows those differences; it does
not yet establish the four installed-consumer checks needed to switch builds.

The shared build uploads `default-lib-<target>`. Binding jobs read the built
Linux library with `just rs package-ffi --bindings-only --library PATH`, so
binding generation does not compile moq-ffi again. Package workflows select
only their own targets, retain language binding names and package outputs,
and keep their existing publisher steps and credentials. Manual calls remain
standalone. Go's PR dry-run also remains standalone.

A target failure must not suppress a package that does not consume that target.
The build matrix and binding matrix do not fail fast; packaging still runs
after a failed shared job and refuses missing artifacts. The wrapper workflows
accept only the existing trusted owner/push/tag inputs. Their read-only
preflight checks the completed coordinator's own language publisher and jobs,
so a failed Android build does not prevent a successful Go wrapper release.
The [jobs API](https://docs.github.com/en/rest/actions/workflow-jobs#list-jobs-for-a-workflow-run)
query includes all attempts and uses each job's latest execution. A publisher
counts only if it ran in the triggering attempt: a partial rerun copies earlier
successes with their original start time, and their wrappers already ran when
that attempt completed. Rerunning only Kotlin on an old tag therefore cannot
re-cut the Go wrapper from old sources after a newer tag published, while a
rerun publisher still releases its wrapper. Missing publisher names or an API
error fail loudly. No publication is polled.
The legacy per-language workflow names remain accepted for in-flight runs.

`sh/size/release.py` reads shared native libraries under the existing Go size
history keys and excludes Android/iOS libraries already counted inside their
language packages. Package artifact names and size comparisons stay stable.

## Evidence and validation

[The sampled Nightly](https://github.com/moq-dev/moq/actions/runs/38564590314)
contains fourteen default-feature native builds. Downloaded library payloads
from all nine uploads for duplicated targets confirm:

| Target | Consumers | Comparison |
| --- | --- | --- |
| x86\_64-unknown-linux-gnu | Go, Kotlin | static and shared libraries have identical SHA-256 hashes |
| aarch64-unknown-linux-gnu | Go, Kotlin | static and shared libraries have identical SHA-256 hashes |
| aarch64-apple-darwin | Go, Kotlin, Swift | static and shared libraries have identical SHA-256 hashes |
| x86\_64-pc-windows-msvc | Go, Kotlin | import library identical; DLL and static library differ only in timestamps and DLL debug GUID |

The Windows DLL has 23 differing bytes, all in PE/COFF timestamps, debug-directory
timestamps and the CodeView GUID. The static archive has 85 differing bytes,
all in COFF timestamps. Zeroing those metadata fields makes both payloads
byte-identical; no executable code or binding metadata differs. The source
build command is the same target/default-feature Cargo invocation for every
consumer. Sharing preserves the same native code and library files.

`just gh test` runs local fixtures for artifact subsets, stable size keys,
build-free binding generation, independent language outcomes, failed API
queries and workflow guards. `just gh check` validates workflow syntax and
alert coverage. The first Nightly after landing measures actual elapsed time;
the sampled five duplicate native builds cost about seventy runner-minutes.

## C++ comparison

This comparison uses the same 2026-10-14 Nightly linked above, at
`0d9e9ff78857003b305f11ebf07095af4c3cdd87`. Compare each `moq-cpp-<target>`
artifact's installed static library with `go-lib-<target>`; those Go libraries
are the default-feature inputs already proved equivalent to Kotlin and Swift.
Preserve this source revision when reproducing the comparison, rather than
mixing artifacts from different runs or rebuilding today's bindings.

Both paths build `moq-ffi` with default `audio` and `video` features,
`--locked`, and the release profile: fat LTO, one codegen unit, and debuginfo
stripped for `moq-ffi`. The run installed Rust 1.99.0 (`b940084d7`) for every
job. C++ invokes `cargo build --locked --package moq-ffi --release` through
CMake; the shared candidate adds `--target <triple>` through
`rs/moq-ffi/build.sh`. C++ is always a native host build. Only the shared ARM
Linux candidate crosses from an x86 Linux host.

| Target | C++ host | Shared candidate host | Native archive comparison |
| --- | --- | --- | --- |
| x86_64-unknown-linux-gnu | Ubuntu 24.04, image 20261004.327.1 | Same image and architecture | 772 common objects byte-identical; one differently named Rust LTO object |
| aarch64-unknown-linux-gnu | Ubuntu 24.04 ARM, image 20261012.149.1 | Ubuntu 24.04 x86, image 20261004.327.1, GCC cross toolchain | 879 common objects byte-identical; one differently named Rust LTO object |
| aarch64-apple-darwin | macOS 26 ARM, image 20260907.0351.1 | Same image and architecture | 847 common objects byte-identical; one differently named Rust LTO object |
| x86_64-pc-windows-msvc | Windows 2025 VS2026, image 20260925.250.1 | Same image and architecture | 665 members each; COFF timestamps and native object build paths differ, plus one differently named Rust LTO object |

The Linux native and cross compilers are GCC 13.3.0. Both runners use Ubuntu
24.04; the ARM cross sysroot is Ubuntu's `libc6-dev-arm64-cross`. The sampled
shared ARM cdylib requires up to GLIBC_2.38 and GLIBCXX_3.4. The installed C++
archive carries unversioned imports, so its glibc minimum cannot be read as a
linked executable's version requirement. Its native objects are nevertheless
byte-identical to the cross build, and the two Linux archive pairs have the
same sets of non-Rust undefined symbols. This rules out a native dependency
change in this sample, without proving an older-distro runtime baseline.

The Windows C++ job identifies MSVC 19.51.36260.0; both jobs use the same
runner image. A sampled AWS-LC object requests `MSVCRT` and `OLDNAMES` in both
archives, preserving the release CRT. Its differing `.debug$S` records contain
`target\release\build\...` versus
`target\x86_64-pc-windows-msvc\release\build\...`; changing the path length
also changes subsequent COFF offsets. Timestamp normalization alone therefore
does not make these C++ archives identical. The remaining Windows members and
the four Rust LTO objects have not been normalized into an executable-code
equivalence proof. No incompatible ABI difference has been established.

Static-library SHA-256 hashes, C++ first and shared candidate second:

```text
x86_64-unknown-linux-gnu
f786f40bd739a7cf97471549bcb30f05f25c35acc14d0b2f826c34a80c9e9505
d663534420b8bea820388c058a01b590eafc2531b8fe72961cdf1afe7ac630bc
aarch64-unknown-linux-gnu
09e0ea5e0ddfd72d4799febba21a22f4d83de4c2b16a08ffa58e9fbfa5ad4f1e
7fcd57f6c4a52332828daf0e6f2d64c9a81082a13df20da023ac3c1d0124577f
aarch64-apple-darwin
1c05406ca3bb8f13f45a9760b5e7eaf5e3fd7eddfeb72f6fa220cd4fb61c74ad
39eaa5f69770ecf6c20dedad43d10891e66a46984cdae3639875cad4235d78fa
x86_64-pc-windows-msvc
6cbbe795ebc6bf992f62ffc4dac98befac2b3f07f523c757f2c9848f395d4ffe
617eb2d358d17af2efe590d0185212fb15aec46e5f4d31318d884155dfe06b86
```

Using the pinned `uniffi-bindgen-cpp` version
`v0.11.0-kixelated.4+v0.32.2` and this revision's `cpp/moq/uniffi.toml`,
all four candidates generate exactly the installed C++ bindings: `moq.hpp`,
`moq.cpp`, `moq_scaffolding.hpp`, and `uniffi_expected.hpp`. Linux and macOS
static archives are readable directly; Windows requires the shared candidate's
DLL because this generator cannot read its static archive's long member names.
Compare generated `moq.cpp` with the installed `include/moq/ffi/moq.cpp`, not
the separate handwritten `share/moq-cpp/moq.cpp` wrapper.

The sampled revision's C++17 consumer probe passes on Linux x86 after linking
the installed headers and wrapper source against the shared candidate static
library. This is a manual experiment using the Nix development toolchain,
not a new CI gate. ARM Linux, macOS and Windows installed consumers have not
run with substituted libraries. Keep the current build until those checks
and the remaining object differences are resolved. PR #5727 separately owns
the generated C++ type/expected shape; repeat this metadata comparison with
its final generator before enabling reuse.

### Release input decision

`cpp-v*` and `moq-ffi-v*` remain separate release lifecycles. The shared build's
default-feature static libraries are Actions artifacts, which expire; an old
C++ release must not depend on those artifacts still existing. Current
`moq-ffi-v*` releases publish Dart's minimal-feature shared libraries and Swift
packages, not standalone default-feature static libraries for all four C++
targets. The minimal-feature assets cannot replace these candidates.

Recommended, pending maintainer direction: record an explicit FFI version in
`cpp/moq/FFI_VERSION` and publish immutable, checksummed default-feature native
assets under that `moq-ffi-v*` release. A C++ release would select that version;
nightly could use the coordinator's same-run artifacts. This adds a release
asset contract and a public build input, so neither is introduced by this
evidence-only change. The alternatives are an explicit workflow-run input
(with artifact expiry making old releases unrebuildable), or retaining native
C++ builds. Settle the release input and CMake/build-script configuration before
implementing reuse; never resolve an absent input to the newest FFI release.
