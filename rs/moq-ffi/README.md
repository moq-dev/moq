# moq-ffi

UniFFI bindings for Media over QUIC (MoQ).

This crate provides Kotlin, Swift, and Python bindings for the MoQ protocol stack via [UniFFI](https://mozilla.github.io/uniffi-rs/). All exported async methods must be called from an appropriate async context (e.g., a Kotlin coroutine scope or Swift `Task`/`async` context).

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

After building, generate language bindings with the included `uniffi-bindgen` binary.

The library extension depends on your platform: `.dylib` (macOS/iOS), `.so` (Linux/Android), `.dll` (Windows).

```bash
cargo run --bin uniffi-bindgen -- generate --library target/release/libmoq_ffi.{dylib,so} --language kotlin --out-dir out/
cargo run --bin uniffi-bindgen -- generate --library target/release/libmoq_ffi.{dylib,so} --language swift --out-dir out/
cargo run --bin uniffi-bindgen -- generate --library target/release/libmoq_ffi.{dylib,so} --language python --out-dir out/
```

## Architecture

```text
moq-ffi (this crate)
├── lib.rs          — Library entry and UniFFI scaffolding
├── ffi.rs          — FFI runtime and Abort helper
├── session.rs      — QUIC session management
├── origin.rs       — Broadcast routing (publish/consume)
├── consumer.rs     — Catalog and track subscription
├── producer.rs     — Broadcast and track publishing
└── error.rs        — Error types
```
