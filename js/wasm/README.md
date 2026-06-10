# @moq/wasm (experiment)

Browser bindings for [`moq-net`](../../rs/moq-net), compiled to WebAssembly with
`wasm-bindgen`. This is the JS-facing half of the `rs/moq-wasm` crate: it
packages the generated bindings so a JS app can `import` the real Rust moq-lite
implementation instead of the hand-written TypeScript one in `@moq/net`.

```ts
import init, { MoqSession, setup } from "@moq/wasm";

await init(); // load the wasm module (wasm-bindgen's default loader)
setup(); // install panic/tracing hooks for readable errors

const session = await MoqSession.connect("https://relay.example.com/anon");
const broadcast = await session.consume("room/alice");
const track = await broadcast?.subscribe("video");
for (let group = await track?.recvGroup(); group; group = await track?.recvGroup()) {
	for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) {
		// frame: Uint8Array
	}
}
```

## Building

`dist/` is generated, not committed. Build it from the repo root:

```bash
just wasm
```

That compiles `rs/moq-wasm` for `wasm32-unknown-unknown`, runs `wasm-bindgen`
(bundler target) into `dist/`, and shrinks the binary with `wasm-opt`. The
required toolchain (wasm target, `wasm-bindgen-cli`, `binaryen`) is provided by
the Nix dev shell.

## Status

This compiles and produces a typed, importable package today. It does **not yet
run** in a browser: `moq-net` calls `tokio::time` directly, which panics on
wasm (no clock / time driver). See [`rs/moq-wasm/README.md`](../../rs/moq-wasm/README.md)
for the remaining work (portable time; media via `moq-mux`).
