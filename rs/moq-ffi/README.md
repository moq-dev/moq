# moq-ffi

UniFFI bindings for Media over QUIC (MoQ).

This crate provides Kotlin, Swift, Python, Go, and Dart bindings for the MoQ protocol stack via [UniFFI](https://mozilla.github.io/uniffi-rs/). All exported async methods must be called from an appropriate async context (for example, a Kotlin coroutine scope, Swift `Task`, or Dart `Future`).

## Building

```bash
cargo build --release --package moq-ffi
```

### iOS

```bash
cargo build --release --package moq-ffi --target aarch64-apple-ios
cargo build --release --package moq-ffi --target aarch64-apple-ios-sim
cargo build --release --package moq-ffi --target x86_64-apple-ios
```

### Android

```bash
cargo ndk -t arm64-v8a build --release --package moq-ffi
```

## Generating bindings

After building, generate language bindings with the `uniffi-bindgen` package.

The library extension depends on your platform: `.dylib` (macOS/iOS), `.so` (Linux/Android), `.dll` (Windows).

```bash
cargo run --package uniffi-bindgen --features cli -- generate --library target/release/libmoq_ffi.{dylib,so} --language kotlin --out-dir out/
cargo run --package uniffi-bindgen --features cli -- generate --library target/release/libmoq_ffi.{dylib,so} --language swift --out-dir out/
cargo run --package uniffi-bindgen --features cli -- generate --library target/release/libmoq_ffi.{dylib,so} --language python --out-dir out/
uniffi_bindgen_dart --library target/release/libmoq_ffi.{dylib,so} --out-dir out/
```

The Dart generator is pinned in `flake.nix`; run `nix develop --command just dart generate` to regenerate the committed package bindings.

## Browser

moq-ffi also builds for `wasm32-unknown-unknown`, where
[ubrn](https://github.com/jhugman/uniffi-bindgen-react-native)'s `wasm2` player binds it
to TypeScript. It is the candidate Rust backend for `@moq/net`, not a published package.
Publishing and consumption over WebTransport work for every moq-lite and moq-transport
version; the codecs, the server, and logging are native-only.

The browser owns the socket and the trust store, so `MoqClientConfig` honors only
`versions`, `tls.fingerprints`, and the origins there, and `MoqClient::new` fails with
`Unsupported` when anything else is set. A session dials once: there is no reconnect and
no WebSocket fallback yet.

ubrn 0.31 reads UniFFI 0.31 metadata, so for now the TypeScript is generated from a
scratch checkout pinned back to UniFFI 0.31:

```bash
just test wasm --uniffi   # build, then run the browser parity cases against real relays
```

[`test/wasm`](../../test/wasm/README.md#generated-bindings) covers the build, the
browser cases, and what `@moq/net` still needs from this surface.

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
