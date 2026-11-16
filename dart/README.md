# Dart and Flutter

This directory contains two packages:

- `moq_ffi` is the generated UniFFI layer. Its version tracks `rs/moq-ffi`.
- `moq` is the independently-versioned ergonomic API applications should use.

Dart is experimental: the API may change in any release. `moq` keeps its own
0.x version instead of the shared minor (`version.json`), until a Dart client
joins the interop suite or the iOS library has run in an iOS app.

The generated layer, `moq_ffi/lib/src/moq.dart` and `uniffi_runtime.dart`, is
not in git. `just dart generate` writes it; run that before a bare
`dart analyze` or `dart test` in a fresh checkout.

Run `just dart check` from the repository root. The check rebuilds
`moq-ffi` in the dev profile, generates the bindings,
analyzes and tests both packages, and validates their publish layout,
including that the `moq_ffi` archive holds the generated files.

`just dart bench` pins an optimized native library through the hook's existing
`library` user define. Published packages download optimized release libraries.

`dart test` drives the Native Assets hook, so the round-trip tests in
`moq/test/` exercise the same load path a Flutter application uses. There is no
example application in-tree: `flutter create` scaffolding is per-platform,
never built by CI, and adds no coverage the tests don't already give.

Flutter Web is out of scope because the Dart UniFFI generator does not support
WebAssembly. Browser applications should use the TypeScript packages.

## Generator

Bindings use [`kixelated/uniffi-dart`](https://github.com/kixelated/uniffi-dart)
tag `v0.3.1-kixelated.7+v0.32.0`, which carries the UniFFI 0.32, library-mode CLI,
disposed-argument, and allocation-failure ownership changes. The Nix development shell supplies that exact
revision plus the `nix/uniffi-dart-*.patch` fixes. Without Nix, install it
without those fixes:

```bash
cargo install --git https://github.com/kixelated/uniffi-dart \
  --tag 'v0.3.1-kixelated.7+v0.32.0' --features binary uniffi-dart
```

## Releases

The raw package follows `moq-ffi-v{{version}}` tags. Its workflow uploads one
checksum-verified native library per supported target, generates the bindings,
and publishes `moq_ffi` with them. The wrapper follows `moq-dart-v{{version}}` tags and publishes
`moq` independently.

Pub.dev requires the first version of each new package to be uploaded manually.
After that bootstrap, configure the `moq-dev/moq` repository and the tag patterns
above under each package's automated publishing settings. Later releases use
GitHub OIDC and store no pub.dev credential.
