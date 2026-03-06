# moq-ffi

UniFFI bindings for Media over QUIC (MoQ).

This crate provides Kotlin and Swift bindings for the MoQ protocol stack via [UniFFI](https://mozilla.github.io/uniffi-rs/). It exposes the same functionality as `libmoq`'s `uniffi-api` feature but as a standalone crate with no feature flags required.

## Building

```bash
cargo build --release --package moq-ffi
```

### iOS

```bash
cargo build --release --package moq-ffi --target aarch64-apple-ios
cargo build --release --package moq-ffi --target aarch64-apple-ios-sim
```

### Android

```bash
cargo ndk -t arm64-v8a build --release --package moq-ffi
```

## Generating bindings

After building, generate language bindings with the included `uniffi-bindgen` binary:

```bash
cargo run --bin uniffi-bindgen -- generate --library target/release/libmoq_ffi.dylib --language swift --out-dir out/
cargo run --bin uniffi-bindgen -- generate --library target/release/libmoq_ffi.dylib --language kotlin --out-dir out/
```

## Architecture

```
moq-ffi (this crate)
├── api.rs          — UniFFI-exported functions and types
├── ffi.rs          — Runtime, callbacks, ID parsing
├── session.rs      — QUIC session management
├── origin.rs       — Broadcast routing (publish/consume)
├── consume.rs      — Catalog and track subscription
├── publish.rs      — Broadcast and track publishing
├── state.rs        — Global shared state
├── error.rs        — Error types
└── id.rs           — Opaque resource identifiers
```
