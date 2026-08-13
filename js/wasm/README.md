# @moq/wasm (experiment)

Browser bindings for [`moq-net`](../../rs/moq-net), compiled to WebAssembly with
`wasm-bindgen`. This is the JS-facing half of the `rs/moq-wasm` crate: it
packages the generated bindings so a JS app can `import` the real Rust moq-lite
and moq-transport implementation instead of the hand-written TypeScript one in
`@moq/net`.

```ts
import init, * as Moq from "@moq/wasm";

await init(); // load the wasm module (wasm-bindgen's default loader)
Moq.setup(); // install panic/tracing hooks for readable errors

const session = await Moq.Session.connect("https://relay.example.com/anon");

// Consume.
const broadcast = await session.announcedBroadcast("room/alice");
const track = await broadcast.subscribe("video");
for (let group = await track.recvGroup(); group; group = await track.recvGroup()) {
	for (let frame = await group.readFrame(); frame; frame = await group.readFrame()) {
		// frame: Uint8Array
	}
}

// Publish.
const room = session.publish("room/bob");
const video = room.createTrack("video");
const group = video.appendGroup();
group.writeFrame(timestampMicros, payload);
group.close();
```

## Surface

The classes mirror `moq-net`'s role modules: `Session`, `BroadcastProducer` /
`BroadcastConsumer`, `TrackProducer` / `TrackConsumer` / `TrackSubscriber` /
`TrackRequest`, `GroupProducer` / `GroupConsumer`, `AnnounceConsumer`, plus the
option bags `Subscription`, `TrackInfo`, and `Frame`. See
[`rs/moq-wasm/README.md`](../../rs/moq-wasm/README.md) for the mapping and for
what is deliberately left out.

Conventions worth knowing before wiring this into app code:

- Durations are milliseconds and timestamps are microseconds, matching `@moq/net`.
- Sequence numbers are `bigint`, since they are `u64` on the wire.
- `close()` is a clean finish; `abort(code)` takes an application close code.
- `closed()` **rejects** rather than resolving, because every close carries a
  reason (a clean one included).
- One in-flight async call per handle. A second concurrent `recvGroup()` on the
  same subscriber throws rather than interleaving; clone the handle instead.

## Building

`dist/` is generated, not committed. Build it from the repo root:

```bash
just wasm
```

That compiles `rs/moq-wasm` for `wasm32-unknown-unknown` and runs `wasm-bindgen`
(web target) into `dist/`. The required toolchain (wasm target and
`wasm-bindgen-cli`) is provided by the Nix dev shell.

## Status

Not a drop-in replacement for `@moq/net` yet. The wire layer works: publish,
consume, discovery, and on-demand serving have all been driven in a browser
against a relay on both `moq-lite-05` and `moq-transport-19`. What is missing is
everything around it.

- **No TS wrapper.** `@moq/net` exposes a reactive, signals-based surface that
  `@moq/watch` and `@moq/publish` build on; these bindings expose Promises. A
  hand-written shim would have to bridge the two.
- **No WebSocket fallback.** `@moq/net` falls back to WebSocket via `@moq/qmux`;
  the Rust `qmux` crate is tokio-based, so there is no wasm path today. Handing
  a JS transport into wasm is the likely fix, since `@moq/qmux` already
  implements the `WebTransport` interface structurally.
- **Bigger.** Roughly 340 KB brotli against about 39 KB for a bundled `@moq/net`.
- **Single-threaded.** Everything runs on the thread that instantiated the
  module. Drain a high-bitrate track in a Worker so decode and render are not
  sharing it.
- **No media muxing.** `moq-mux` is not wasm-ready.
