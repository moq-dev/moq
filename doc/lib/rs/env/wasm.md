---
title: WebAssembly
description: Compiling a Rust MoQ client to wasm32 and running it in the browser
---

# WebAssembly

[`moq-net`](/lib/rs/crate/moq-net) compiles to `wasm32-unknown-unknown` and runs
inside a browser tab. There it reuses the browser's native `WebTransport` instead
of bringing its own QUIC stack, so the wasm bundle stays small. Reach for this
when you want to share Rust networking and media logic between a native app and
the web, rather than maintaining a separate [TypeScript](/lib/js/) client.

If you just want MoQ in a web page and aren't already invested in Rust, the
[TypeScript libraries](/lib/js/) are the easier path. They're the reference
browser implementation.

## How it fits together

`moq-net` talks to anything that implements [`web_transport_trait::Session`](https://docs.rs/web-transport-trait).
The [`web-transport`](https://crates.io/crates/web-transport) meta-crate picks
the backend by target automatically:

- **native** &rarr; [`web-transport-quinn`](https://crates.io/crates/web-transport-quinn) (a real QUIC stack)
- **wasm** &rarr; [`web-transport-wasm`](https://crates.io/crates/web-transport-wasm) (a thin wrapper over the browser's `WebTransport`)

So the same `moq-net` code works in both places. On wasm you skip
[`moq-native`](/lib/rs/crate/moq-native) entirely (it's quinn-only) and get the
`Session` from `web-transport` instead.

::: warning No `Send` on wasm
Browser wasm is single-threaded and its `WebTransport` handles aren't `Send`,
so the futures driving the transport can't be `Send` either. Spawn them on a
local task (`wasm_bindgen_futures::spawn_local`) rather than a multi-threaded
executor like a default `tokio` runtime.
:::

## Setup

```toml
# Cargo.toml
[dependencies]
moq-net = "0.1"
web-transport = "0.10"
url = "2"
wasm-bindgen = "0.2"
wasm-bindgen-futures = "0.4"
```

Build with [`wasm-pack`](https://rustwasm.github.io/wasm-pack/) or
[`trunk`](https://trunkrs.dev/):

```bash
wasm-pack build --target web
# or, for a full app
trunk serve
```

## Connecting

Build a transport `Session` with `web-transport`, then hand it to
[`moq_net::Client`](https://docs.rs/moq-net/latest/moq_net/struct.Client.html).
From there the pub/sub API is identical to [native](/lib/rs/env/native):

```rust
let url = url::Url::parse("https://relay.moq.dev/anon")?;

// On wasm this wraps the browser's native WebTransport.
let transport = web_transport::ClientBuilder::new()
    .with_system_roots()
    .connect(url)
    .await?;

// Hand the transport to moq-net and run the MoQ handshake.
let origin = moq_net::Origin::new().produce();
let mut consumer = origin.consume();
let session = moq_net::Client::new()
    .with_consume(origin)
    .connect(transport)
    .await?;

// Read announcements off `consumer` exactly as on native...
```

Only the transport setup differs from [native](/lib/rs/env/native); the
[`Origin`](https://docs.rs/moq-net/latest/moq_net/struct.Origin.html),
broadcast, track, group, and frame APIs are the same.

## Next steps

- [Native environment](/lib/rs/env/native) - The same API over a real QUIC stack
- [moq-net](/lib/rs/crate/moq-net) - Core networking crate
- [TypeScript libraries](/lib/js/) - The reference browser client
