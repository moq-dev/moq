# Dart and Flutter

This directory contains two packages:

- `moq_ffi` is the generated UniFFI layer. Its version tracks `rs/moq-ffi`.
- `moq` is the independently-versioned ergonomic API applications should use.

Run `just dart check` from the repository root. The check rebuilds
`moq-ffi` without optional codec features, verifies the generated source,
analyzes and tests both packages, and validates their publish layout.

`dart test` drives the Native Assets hook, so the round-trip tests in
`moq/test/` exercise the same load path a Flutter application uses. There is no
example application in-tree: `flutter create` scaffolding is per-platform,
never built by CI, and adds no coverage the tests don't already give.

Flutter Web is out of scope because the Dart UniFFI generator does not support
WebAssembly. Browser applications should use the TypeScript packages.

## Generator

Bindings use [`kixelated/uniffi-dart`](https://github.com/kixelated/uniffi-dart)
tag `v0.3.0+v0.32.0`, which carries the UniFFI 0.32 and library-mode CLI
changes. The Nix development shell supplies that exact revision. Without Nix,
install it with:

```bash
cargo install --git https://github.com/kixelated/uniffi-dart \
  --tag 'v0.3.0+v0.32.0' --features binary uniffi-dart
```

## Releases

The raw package follows `moq-ffi-v{{version}}` tags. Its workflow uploads one
checksum-verified native library per supported target before publishing
`moq_ffi`. The wrapper follows `moq-dart-v{{version}}` tags and publishes
`moq` independently.

Pub.dev requires the first version of each new package to be uploaded manually.
After that bootstrap, configure the `moq-dev/moq` repository and the tag patterns
above under each package's automated publishing settings. Later releases use
GitHub OIDC and store no pub.dev credential.
