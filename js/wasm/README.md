# @moq/wasm (experiment)

Browser bindings for [`moq-net`](../../rs/moq-net), compiled to WebAssembly with
`wasm-bindgen`. This is the JS-facing half of the `rs/moq-wasm` crate: it
packages the generated bindings so a JS app can `import` the real Rust moq-lite
implementation instead of the hand-written TypeScript one in `@moq/net`.

```ts
import init, * as Moq from "@moq/wasm";

await init(); // load the wasm module (wasm-bindgen's default loader)
Moq.setup(); // install panic/tracing hooks for readable errors

const session = await Moq.Session.connect("https://relay.example.com/anon");
const broadcast = await session.consume("room/alice");
const track = await broadcast?.subscribe("video");
for (let group = await track?.recvGroup(); group; group = await track?.recvGroup()) {
	for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) {
		// frame: Uint8Array
	}
}
```

The classes (`Moq.Session`, `Moq.Broadcast`, `Moq.Track`, `Moq.Group`) drop the
`Moq` prefix since they're already namespaced under the import.

## Building

`dist/` is generated, not committed. Build it from the repo root:

```bash
just wasm
```

That compiles `rs/moq-wasm` for `wasm32-unknown-unknown`, runs `wasm-bindgen`
(web target) into `dist/`, and shrinks the binary with `wasm-opt`. The required
toolchain (wasm target, `wasm-bindgen-cli`, `binaryen`) is provided by the Nix
dev shell.

## Status

Compiles and produces a typed, importable package. The consume path is
runtime-portable: `moq-net`'s timers and `Instant` go through `web_async::time`
(wasmtimer on wasm), so they no longer panic. Not yet exercised end-to-end in a
browser against a relay, and media muxing (`moq-mux`) is still out. See
[`rs/moq-wasm/README.md`](../../rs/moq-wasm/README.md) for details.
