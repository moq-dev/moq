# [L] Dart/Flutter bindings via UniFFI

## Goal

Implement and verify the behavior tracked in [#3100](https://github.com/moq-dev/moq/issues/3100)
within the issue's stated scope and boundaries.

## Plan

Being executed in [moq#3215](https://github.com/moq-dev/moq/pull/3215), which
delivers every phase below and carries the closing keyword: the pinned UniFFI
0.32 generator fork, the two-package `dart/` layout, the ergonomic wrapper,
checksum-verified native assets, the CI and release workflows, and
`doc/lib/dart`. Delete this quest when that PR lands, and clear the blockers
it leaves behind on [the leak](/quest/m0/dart-leak.md) and
[codec parity](/quest/m2/dart-codecs.md).

The original plan follows, as the record of what was decided.

### Issue context

`rs/moq-ffi` is already the single UniFFI core behind five wrappers (`py/`, `swift/`, `kt/`, `go/`, `rs/libmoq`). Dart/Flutter would be a sixth of the same shape. It is viable, but it carries the same cost Go does: the Dart frontend is a third-party bindgen that lags uniffi-rs, so we would own a pinned fork.

#### Landscape (Aug 2026)

Two generators exist, neither maintained by Mozilla:

| | [Uniffi-Dart/uniffi-dart](https://github.com/Uniffi-Dart/uniffi-dart) (ex-acterglobal) | [nchapman/uniffi-bindgen-dart](https://github.com/nchapman/uniffi-bindgen-dart) |
|---|---|---|
| Activity | commits 2026-08-22, 43 stars, 25 open issues | last push 2026-03-04, 3 stars, 199 downloads |
| uniffi target | 0.31.2 (`v0.2.1+v0.31.2`) | 0.31.0 |
| CLI | none, `src/bin.rs` is a placeholder stub | real CLI with library mode + `doctor` |
| Library loading | `@Native(assetId:)`, so Dart native assets | `DynamicLibrary.open` + `configureDefaultBindings()` |
| Coverage | 30 fixtures: proc-macros, async, objects, records, enums, errors, maps, bytes, traits, callbacks | similar claims, no fixture suite |

Proposal: **uniffi-dart**, on maintenance grounds. Its README's "5 critical blockers" list is stale. `MapCodeType` is implemented in `src/gen/compounds.rs` and `fixtures/proc-macro/` exists, so `HashMap` and proc-macros both work.

#### Fit against the actual `moq-ffi` surface

33 `uniffi::Object`, 25 `Record`, 6 `Enum`, one `flat_error` enum, 45 `pub async fn`, `HashMap<String, V>` in `MoqCatalog`, `Vec<u8>` frames. All covered. We use **zero** callback interfaces and zero trait interfaces, which avoids uniffi-dart's weakest area entirely.

Async works across threads: `src/gen/types.rs` emits `NativeCallable<UniffiRustFutureContinuationCallback>.listener(...)`, so completions from the dedicated `moq-ffi` tokio thread land correctly on the Dart isolate. That was the main technical risk and it is already handled upstream.

Flutter packaging is no longer the obstacle it once was. Build hooks (`hook/build.dart`) and code assets are stable as of Dart 3.10 / Flutter 3.38; link hooks and tree-shaking landed in 3.13. Cargokit was archived 2026-03-26 in favor of exactly this, and `native_toolchain_rust` drives cargo from a build hook.

#### The blocker

`moq-ffi` is on uniffi **0.32**; uniffi-dart targets **0.31.2**. Per #2946 / #2949, `UNIFFI_CONTRACT_VERSION` stayed at 30 while the metadata encoding changed, so a 0.31 bindgen dies on the first symbol. Upstream [#149](https://github.com/Uniffi-Dart/uniffi-dart/pull/149) (uniffi 0.32) and [#152](https://github.com/Uniffi-Dart/uniffi-dart/pull/152) (library-mode CLI) are open, both by DenisovAV, both `mergeable_state: unstable`, last updated 2026-08-23.

The library API `gen::generate_dart_bindings(..., library_mode: true)` already works on `main`; only the *binary* is missing. Worst case we write a 30-line bin like `rs/moq-ffi/uniffi-bindgen.rs`.

#### Plan

##### Phase 0: spike, gate everything else on it (~1 day)

Cherry-pick #149 + #152 + #151 onto a branch, build `moq-ffi` with `--no-default-features`, generate against `libmoq_ffi.dylib`, run `dart analyze` on the output, and drive one connect + announce + subscribe round-trip against a local relay from a plain Dart CLI. If the generated Dart does not analyze clean or async deadlocks, stop and reconsider.

##### Phase 1: the generator

Publish `kixelated/uniffi-dart` tag `v0.3.0+v0.32.0`, mirroring the `kixelated/uniffi-bindgen-go` arrangement. Add a `buildRustPackage` derivation next to `uniffi-bindgen-go` in `flake.nix`. The repo + tag will need naming in the same handful of places: `flake.nix`, `dart/scripts/check.sh`, the release workflow env, `dart/README.md`, `doc/lib/dart/`.

Test the *unlocked* `cargo install --git` resolve explicitly. That is where the `toml = ">=0.9, <2"` range bit us on #2949, and no local path (cargo build, nix vendoring, the fixture suite) exercises it.

##### Phase 2: `dart/` layout

Mirror the `kt/` two-package split:

```
dart/
  moq_ffi/    # generated bindings + hook/build.dart, version tracks the crate
  moq/        # ergonomic wrapper, versioned independently
  scripts/{check,package,publish}.sh
  justfile    # `mod dart` in the root justfile
```

Open decision: does `hook/build.dart` **compile** Rust on the consumer's machine (requires their Rust toolchain, ~5 min first build) or **download** a prebuilt from the existing `moq-ffi-v*` release assets? Leaning download-with-cargo-fallback, matching how Swift ships an XCFramework and Kotlin a Maven `.so`.

Ship `--no-default-features` first. `audio` / `video` drag in ~40 crates plus openh264's vendored C++.

##### Phase 3: ergonomic `moq` package

Port `kt/moq/src/jvmAndAndroidMain/kotlin/dev/moq/Moq.kt`: `Moq.connect()`, announcements as a Dart `Stream` rather than a poll loop, `Future`-based everything, `close()`. Plus a Flutter example app.

##### Phase 4: CI and release

`just dart check` skipping unless the diff touches `dart/` or `rs/moq-ffi/`, a `_tools` entry for `MOQ_STRICT`, `dart` + `flutter` in the devShell, and `release-dart-ffi.yml` / `release-dart.yml` modeled on the `release-kt-*` pair. pub.dev accepts GitHub Actions OIDC publishing, so there is no token to manage.

##### Phase 5: docs

`doc/lib/dart/`, a `dart/` entry in the CLAUDE.md Cross-Package Sync row for `rs/moq-ffi`, and `doc/lib/index.md`.

#### Explicitly out of scope for phase 1

- **Flutter Web.** uniffi-dart has no wasm support ([PR #126](https://github.com/Uniffi-Dart/uniffi-dart/pull/126) is dirty and stalled since May). Point web users at the JS packages.
- **Video rendering.** `moq-ffi` hands back decoded frames, not a Flutter `Texture`. A texture bridge is per-platform plugin work and a separate project. Phase 1 is data-plane only: connect, announce, publish/subscribe, catalog, raw frames.

#### Costs

We would own a second bindgen fork, on a repo badged "experimental" with 43 stars and effectively one maintainer plus two drive-by contributors. Every `moq-ffi` change would then ripple to six wrappers instead of five.

The alternative generator, flutter\_rust\_bridge, is worse for us specifically: it wants its own Rust-side API definition, so it duplicates `moq-ffi` rather than reusing it, which cuts against the single-core rule.

#### References

- [Uniffi-Dart/uniffi-dart](https://github.com/Uniffi-Dart/uniffi-dart)
- [nchapman/uniffi-bindgen-dart](https://github.com/nchapman/uniffi-bindgen-dart)
- [Dart hooks](https://dart.dev/tools/hooks)
- [code\_assets](https://pub.dev/packages/code_assets)
- [native\_toolchain\_rust](https://pub.dev/packages/native_toolchain_rust)
- [flutter\_rust\_bridge: migrate from Cargokit to native assets](https://cjycode.com/flutter_rust_bridge/manual/integrate/migrate-cargokit-to-native-assets)

## Closes

- [#3100](https://github.com/moq-dev/moq/issues/3100) - close this issue when the quest finishes
