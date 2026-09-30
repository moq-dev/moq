# [XS] Android logs always go to logcat

## Goal

On dev, `moq_tokio::Log::init` sends logs to logcat on every Android build, so
the published Kotlin and Dart bindings (and libmoq) stop losing every Rust log
to stderr, which Android app processes discard. The `android-logcat` Cargo
feature is gone from `moq-tokio` and `moq-ffi`.

## Plan

Today the logcat layer in `rs/moq-tokio/src/log.rs` is gated on
`all(target_os = "android", feature = "android-logcat")`, and no shipped build
enables the feature: libmoq has no `[features]`, and `rs/moq-ffi/build.sh`, the
Dart build hook, and the release workflows build Android without it. The
published `dev.moq:moq-ffi-android` 0.4.8 does not link `liblog`.

Decided (maintainer, 2026-09-30): gate the layer on `target_os = "android"`
alone and delete the feature, rather than wire it through every build, so no
Android consumer has to know to turn it on. Logging stays opt-in at runtime,
since `Log::init` runs only when the app calls it. Removing a feature from
published crates is a break, so this targets dev; no no-op feature is kept.

- `tracing-android` becomes a plain dependency under the existing
  `cfg(target_os = "android")` target table in `rs/moq-tokio/Cargo.toml`; it
  must stay target-gated because `android_log-sys` links `liblog`
  unconditionally.
- Fix the `Log::init` doc comment, which already claims logcat.
- Leave the `rs/moq-native` tombstone alone.
- Regression check: the Android CI workflow builds moq-ffi for one Android
  target and asserts the `.so` imports `__android_log_*`.

The reporter ([#4456](https://github.com/moq-dev/moq/issues/4456)) has this
working locally and offered the PR.

Public API: breaking on dev, the `android-logcat` feature is removed from
`moq-tokio` and `moq-ffi`. Wire: none.

## Closes

- [#4456](https://github.com/moq-dev/moq/issues/4456) - Rust logs never reach logcat on Android
