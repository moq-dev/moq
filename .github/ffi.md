# Shared FFI releases

`release-shared-ffi.yml` owns the existing `moq-ffi-v*` tag trigger for Go,
Kotlin and Swift. Nightly calls the same coordinator without publisher secrets.
It builds nine distinct default-feature targets, replacing fourteen native
builds across those packages. Dart retains its own eleven default-feature
targets, a superset that adds `x86_64-apple-darwin` and `x86_64-apple-ios`;
Python retains its wheel builds and Python-specific environment.

C++ retains source builds as its default and independent `cpp-v*` release tags.
Nightly packages reuse the coordinator's validated same-run native archives.
The [historical C++ comparison](#c-comparison) records differing archive hashes,
including native versus cross-built ARM Linux inputs. The
[current installed-consumer proof](#current-installed-consumer-proof) separately
validates the prebuilt path on all four C++ hosts. Pinned C++ release reuse
still awaits real published default-feature assets and validation of their
exact version and checksum pins.

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
The legacy per-language workflow names remain accepted: a tag cut from a
`release` that predates the coordinator still runs the per-language workflows.

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
alert coverage.

[The first Nightly after landing](https://github.com/moq-dev/moq/actions/runs/38897397428)
measured the saving against the sample:

| Jobs | Sampled | Shared |
| --- | --- | --- |
| Default-feature native builds | 14 jobs, 186 runner-minutes | 9 jobs, 116 runner-minutes |
| Binding generation | 3 jobs, 40 runner-minutes | 3 jobs, 19 runner-minutes |

That is about 90 runner-minutes saved per run. Binding generation now waits for
every native build, since it reads the Linux library; Go's binding job spends
most of its 12 minutes compiling `uniffi-bindgen-go`. The Go, Kotlin and Swift
packagers each selected their own targets from the shared artifacts and passed.

## C++ comparison

This comparison uses the sampled 2026-10-14 Nightly linked above, at
`0d9e9ff78857003b305f11ebf07095af4c3cdd87`. Compare each `moq-cpp-<target>`
artifact's installed static library with `go-lib-<target>`; those Go libraries
are the default-feature inputs already proved equivalent to Kotlin and Swift.
Preserve this source revision when reproducing the comparison, rather than
mixing artifacts from different runs or rebuilding today's bindings.

Both paths build `moq-ffi` with default `audio` and `video` features,
`--locked`, and the release profile: fat LTO, one codegen unit, and debuginfo
stripped for `moq-ffi`. The toolchain action installed stable Rust 1.99.0,
but the repository's `rust-toolchain.toml` selects Rust 1.98.1 (`48a229cea`)
for the actual Cargo builds on both paths. C++ invokes
`cargo build --locked --package moq-ffi --release` through
CMake; the shared candidate adds `--target <triple>` through
`rs/moq-ffi/build.sh`. C++ is always a native host build. Only the shared ARM
Linux candidate crosses from an x86 Linux host.

| Target | C++ host | Shared candidate host | Native archive comparison |
| --- | --- | --- | --- |
| x86\_64-unknown-linux-gnu | Ubuntu 24.04, image 20261004.327.1 | Same image and architecture | 772 common objects byte-identical; one differently named Rust LTO object |
| aarch64-unknown-linux-gnu | Ubuntu 24.04 ARM, image 20261012.149.1 | Ubuntu 24.04 x86, image 20261004.327.1, GCC cross toolchain | 879 common objects byte-identical; one differently named Rust LTO object |
| aarch64-apple-darwin | macOS 26 ARM, image 20260907.0351.1 | Same image and architecture | 847 common objects byte-identical; one differently named Rust LTO object |
| x86\_64-pc-windows-msvc | Windows 2025 VS2026, image 20260925.250.1 | Same image and architecture | 6,077 members each, preserving duplicate basenames; timestamps, debug paths and six assembly code sections differ, plus one differently named Rust LTO object |

The Linux native and cross compilers are GCC 13.3.0. Both runners use Ubuntu
24.04; the ARM cross sysroot is Ubuntu's `libc6-dev-arm64-cross`. The sampled
shared ARM cdylib requires up to GLIBC\_2.38 and GLIBCXX\_3.4. The installed C++
archive carries unversioned imports, so its glibc minimum cannot be read as a
linked executable's version requirement. Its native objects are nevertheless
byte-identical to the cross build, and the two Linux archive pairs have the
same sets of non-Rust undefined symbols. This rules out a native dependency
change in this sample, without proving an older-distro runtime baseline.

The Windows C++ job identifies MSVC 19.51.36260.0; both jobs use the same
runner image. A sampled AWS-LC object requests `MSVCRT` and `OLDNAMES` in both
archives, preserving the release CRT. Preserving duplicate archive member
names gives 6,077 members per archive; the earlier 665 count was distinct
basename keys. Of 6,076 matched members, 5,685 are identical, 43 differ only
in timestamps, and 342 differ only in debug/discarded sections and their
associated auxiliary symbols, timestamps and offsets. Their `.debug$S`
records contain `target\release\build` versus
`target\x86_64-pc-windows-msvc\release\build` paths.

Six OpenH264 assembly members have executable `.text` differences: `dct.o`,
`downsample_bilinear.o`, `matrix_transpose.o`, `mb_copy.o`, `mc_luma.o`, and
the second `vaa.o`. For `matrix_transpose.o`, `mb_copy.o`, and the second
`vaa.o`, normalized instruction listings match after accounting for
CDQE versus MOVSXD sign extension and alignment padding. The other three
remain unproven. The C++ job installed NASM 3.2.0; the shared assembler's
version was not logged, so a tool-version cause is plausible, not established.

On each Linux target, all 427 `.text.uniffi_*` and 295
`.rodata.UNIFFI_META*` payloads match. Their relocations and internal symbols
differ, so equal section payloads do not prove linked equivalence. None of
the four Rust fat-LTO objects has a complete executable-code equivalence
proof. No incompatible ABI difference has been established; installed
consumers must still execute against the actual selected prebuilt libraries.

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

The sampled revision's C++17 consumer probe passed on Linux x86 after linking
the installed headers and wrapper source against the shared candidate static
library. That manual experiment used the Nix development toolchain. ARM Linux,
macOS and Windows consumers were not run with that historical sample; it alone
did not justify reuse. The current-source proof below tests all four hosts
without claiming byte-for-byte executable equivalence.

### Current installed-consumer proof

The [2026-10-16 focused dry-run](https://github.com/moq-dev/moq/actions/runs/38969347067)
passed at implementation commit `cbbcf9b2c92aa88c5550d9175adcc317f8137f4a`.
All four shared default-feature release builds passed, followed by installed
C++17 and C++23 consumers on Linux x86, Linux ARM, macOS ARM and Windows MSVC.
Both C++23 forms ran their coroutine probes. Unix also passed pkg-config
consumers and compiled the documented examples. Each job then archived,
verified and extracted its checked input and produced its final C++ package.
The consumer and package jobs did not rebuild moq-ffi with Cargo. Release and
OBS publication jobs were skipped.

This validates the same-run prebuilt path for that coherent source, generator
and configuration on those runners. No published release pin has been tested,
and this does not establish an older Linux runtime baseline. Documentation-only
evidence updates after that implementation commit do not change the tested code.

### Release input decision

`cpp-v*` and `moq-ffi-v*` remain separate release lifecycles. The shared build's
default-feature static libraries are Actions artifacts, which expire; an old
C++ release must not depend on those artifacts still existing. Current
`moq-ffi-v*` releases publish Dart's minimal-feature shared libraries and Swift
packages, not standalone default-feature static libraries for all four C++
targets. The minimal-feature assets cannot replace these candidates.

The maintainer approved an explicit `cpp/moq/FFI_VERSION` pin selecting
checksummed default-feature assets on the corresponding `moq-ffi-v*` release.
`FFI_SHA256.json` pins every selected archive by its complete filename and
SHA-256. There is no newest-release lookup. The pin is intentionally unset:
existing releases lack this four-target asset contract. Source builds remain
the default until a real release publishes all four validated assets and a
subsequent pin change supplies their exact hashes. A malformed pin, missing
asset, wrong hash, wrong feature/profile, or incompatible target fails.

### Native input and release gates

`MOQ_FFI_DIR` in CMake and `--ffi-dir DIR` in `just cpp package` select an
extracted default-feature native input. Its manifest names the FFI version,
source commit, target, feature set and profile; it carries the static library,
metadata cdylib and native linker inputs together. CMake generates bindings
from that cdylib without invoking Cargo. Direct directories are explicit local
or same-run inputs; release downloads additionally require the checked-in hash.
Without an explicit directory, `just cpp package` fetches exactly the pinned
release input. The pin selects package-release inputs; direct CMake builds and
`just cpp check` build the current checkout unless `MOQ_FFI_DIR` or `--ffi-dir`
is explicitly supplied. A failed selected release fetch never builds source.

The shared coordinator downloads its four exact `default-lib-<target>`
artifacts on Ubuntu x86, Ubuntu 24.04 ARM, macOS ARM and Windows MSVC. Each runs
`just cpp check --ffi-dir DIR`: install the package, then build and execute the
existing C++17 and C++23 consumers, including the standard set on the consumer
target, with exceptions/RTTI disabled. Unix also tests installed pkg-config
linking and compiles the documented examples. No prebuilt error invokes Cargo.
The cross-built ARM input therefore executes on an ARM host. This checks that
runner's baseline only; it does not establish an older Linux runtime baseline.

Only after all four consumers succeed can the tag coordinator publish
`moq-ffi-default-<version>-<target>.tar.gz` and one `.sha256` per archive.
The publisher refuses changed bytes under an existing asset name and never
clobbers assets. These names are separate from Dart's minimal-feature assets.
A first publication is an external condition for selecting a release pin;
this change creates no release or tag. After publication, validate the actual
pinned downloads through the same four-host consumer path before activation.

A branch-only `release-cpp.yml` dispatch with `prebuilt-test=true` builds just
these four shared targets and exercises the prebuilt package and installed
consumers without publishing. Nightly waits for the shared coordinator, then
packages each target from its
`cpp-native-<target>` artifact, which exists only after that target's installed
consumers succeed. It verifies the archive sidecar and manifest before CMake
uses it, with no source fallback. An unrelated coordinator failure does not
suppress a successful C++ target. Tag releases refuse same-run inputs and keep
the explicit release pin path. `just cpp check` and `just gh test` also
cover hash refusal,
archive traversal/symlink refusal, reproducible packaging, publication refusal,
and CMake's build-free prebuilt path with malformed-input fixtures.

PR #5727 independently changes the generator and overlaps the C++ checks and
release workflow. Today's matrix validates this checkout's coherent generator
and config. If that PR lands before this change or the selected release, rerun
the same gates on the final checkout. The old sampled binding matches alone
do not validate a new generator.
