---
title: Rust Environments
description: Where Rust MoQ clients run, native and WebAssembly
---

# Rust Environments

The same Rust crates ([`moq-net`](/lib/rs/crate/moq-net), [`hang`](/lib/rs/crate/hang))
target two very different environments. `moq-net` is transport-agnostic: it runs
over anything that implements [`web_transport_trait::Session`](https://docs.rs/web-transport-trait),
so the only thing that changes between environments is which transport you hand it.

## [Native](/lib/rs/env/native) <Badge type="tip" text="server, desktop, mobile" />

Servers, CLIs, desktop apps, and mobile (via the [FFI bindings](/lib/)). Uses
[`moq-native`](/lib/rs/crate/moq-native) to stand up a QUIC endpoint with
[quinn](https://crates.io/crates/quinn) and [rustls](https://crates.io/crates/rustls),
with automatic WebSocket fallback. This is the common case.

[Native guide](/lib/rs/env/native)

## [WebAssembly](/lib/rs/env/wasm) <Badge type="tip" text="browser" />

Compile a Rust client to `wasm32-unknown-unknown` and run it inside a browser
tab, reusing the browser's own `WebTransport`. Useful when you want to share
Rust logic between native and web instead of maintaining a separate
[TypeScript](/lib/js/) client.

[WASM guide](/lib/rs/env/wasm)
