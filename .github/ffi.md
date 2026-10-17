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
