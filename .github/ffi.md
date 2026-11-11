# Shared FFI releases

`release-shared-ffi.yml` (Release FFI) owns the `moq-ffi-v*` tag trigger for
Go, Kotlin and Swift. Nightly calls the same coordinator without publisher
secrets. It builds each default-feature target once and uploads
`default-lib-<target>`; binding jobs read the built Linux library with
`just rs package-ffi --bindings-only --library PATH` instead of compiling
moq-ffi again. Package workflows select only their own targets and keep their
own binding names, outputs, publisher steps and credentials. Manual calls and
Go's PR dry-run stay standalone. Dart keeps its own superset of targets; Python
keeps its own wheel builds. Sharing produces the same native code as per-language
builds ([#5549](https://github.com/moq-dev/moq/pull/5549)).

## Failure isolation

- A target failure must not suppress a package that does not consume it. The
  build and binding matrices do not fail fast; packaging refuses missing
  artifacts.
- Wrapper workflows (`release-go.yml`, `release-swift-lib.yml`) accept only the
  trusted owner/push/tag inputs. Their preflight asks the
  [jobs API](https://docs.github.com/en/rest/actions/workflow-jobs#list-jobs-for-a-workflow-run)
  whether the coordinator's own language publisher succeeded, so another
  language's failure does not block them.
- A publisher counts only if it ran in the triggering attempt. A partial rerun
  copies earlier successes with their original start time, so rerunning one
  language on an old tag cannot re-cut another's wrapper from old sources
  ([#5573](https://github.com/moq-dev/moq/pull/5573)).
- Missing publisher names or an API error fail loudly. No publication is polled.

`sh/size/release.py` reads shared native libraries under the existing Go size
history keys and excludes Android/iOS libraries already counted inside their
language packages. `just gh test` covers these rules with local fixtures.

## C++

C++ builds from source by default and keeps independent `cpp-v*` tags.
`MOQ_FFI_DIR` (CMake) or `--ffi-dir DIR` (`just cpp package`) selects an
extracted default-feature native input whose manifest names its version,
commit, target, features and profile; CMake then generates bindings without
Cargo. `cpp/moq/FFI_VERSION` pins a `moq-ffi-v*` release and `FFI_SHA256.json`
pins each archive's hash. The pin stays unset until a release publishes all four
validated assets, and is set only after those downloads pass the same four-host
consumer check; publication checks same-run inputs, not the downloads. A malformed pin, missing asset, wrong hash, wrong
feature/profile or incompatible target fails; a failed fetch never falls back to
source.

The coordinator runs `just cpp check --ffi-dir DIR` on Linux x86, Linux ARM,
macOS ARM and Windows MSVC. Only after all four installed consumers pass may a
tag publish `moq-ffi-default-<version>-<target>.tar.gz` with a `.sha256` each;
the publisher never clobbers or changes an existing asset. Nightly packages C++
from these same-run `cpp-native-<target>` artifacts; tag releases refuse
same-run inputs, using the pin when set and building from source otherwise. Archive comparisons and the four-host consumer
proof are in [#5740](https://github.com/moq-dev/moq/pull/5740).
