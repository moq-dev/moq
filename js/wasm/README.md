# @moq/wasm (experiment)

Browser bindings for [`moq-net`](../../rs/moq-net), compiled to WebAssembly with
`wasm-bindgen`. This is the JS-facing half of the `rs/moq-wasm` crate: it lets a
JS app `import` the real Rust moq-lite implementation instead of the
hand-written TypeScript one in `@moq/net`, so the wire protocol lives in exactly
one place (Rust).

The package is a thin hand-written TypeScript shim (`src/`) over the wasm-bindgen
output (`dist/`). The shim presents the **same surface as `@moq/net`** so it can
be a drop-in replacement: the `Connection` / `Path` / `Time` namespaces, the
`Broadcast` / `Track*` / `Group` model classes, the string/json/bool
conveniences, options-object signatures, a reactive `state.closed` signal, and
`number` (not `bigint`) sequences. `Path` and `Time` are re-exported from
`@moq/net` (they carry no wire code, so a bundler tree-shakes the rest and the
branded types stay identical).

```ts
import * as Moq from "@moq/wasm";

const conn = await Moq.Connection.connect(new URL("https://relay.example.com/anon"));

// Consume
const broadcast = conn.consume("room/alice"); // synchronous; subscribing waits for the announce
const track = await broadcast.subscribe("video");
for (let group = await track.recvGroup(); group; group = await track.recvGroup()) {
	for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) {
		// frame: Uint8Array
	}
}

// Publish
const out = new Moq.Broadcast();
conn.publish("room/me", out);
for (let req = await out.requested(); req; req = await out.requested()) {
	const producer = req.accept();
	const g = producer.appendGroup();
	g.writeFrame(new Uint8Array([1, 2, 3]));
	g.close();
}
```

## Building

`dist/` is generated, not committed. Build it from the repo root:

```bash
just wasm
```

That compiles `rs/moq-wasm` for `wasm32-unknown-unknown`, runs `wasm-bindgen`
(web target) into `dist/`, and shrinks the binary with `wasm-opt`. The required
toolchain (wasm target, `wasm-bindgen-cli`, `binaryen`) is provided by the Nix
dev shell. The shim loads the wasm lazily on the first `Connection.connect`.

## Status

The consume **and** publish paths are bound (including real announce discovery
via the `OriginConsumer`), and `@moq/watch` / `@moq/publish` now import
`@moq/wasm` directly. `moq-net`'s timers and `Instant` go through
`web_async::time` (wasmtimer on wasm), so they don't panic. Still pending:
end-to-end exercise in a browser against a relay, bandwidth/RTT telemetry
(declared but undefined), and media muxing (`moq-mux` is not yet wasm-ready). See
[`rs/moq-wasm/README.md`](../../rs/moq-wasm/README.md) for details.
