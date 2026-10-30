import { Milli } from "@moq/clock";
import type * as Moq from "@moq/net";
import type * as Wasm from "@moq/wasm";
import { type Sample, verify } from "./fixture.ts";
import type { Uniffi } from "./uniffi.ts";

/** The relay's self-signed certificate hash, and its URL rewritten for WebTransport. */
async function pin(relay: string): Promise<{ url: URL; hex: string }> {
	const url = new URL(relay);
	const response = await fetch(new URL("/certificate.sha256", url));
	if (!response.ok) throw new Error(`certificate HTTP ${response.status}`);
	const hex = (await response.text()).trim();
	if (!/^[a-fA-F0-9]{64}$/.test(hex)) throw new Error("malformed certificate hash");
	url.protocol = "https:";
	return { url, hex };
}

/** Record the next instantiated module's memory; ubrn's player keeps its instance private. */
function captureMemory(): () => WebAssembly.Memory | undefined {
	let memory: WebAssembly.Memory | undefined;
	const original = WebAssembly.instantiate;
	WebAssembly.instantiate = (async (...args: Parameters<typeof original>) => {
		const result = (await original(...args)) as WebAssembly.Instance | WebAssembly.WebAssemblyInstantiatedSource;
		const instance = result instanceof WebAssembly.Instance ? result : result.instance;
		memory ??= Object.values(instance.exports).find((value) => value instanceof WebAssembly.Memory);
		return result;
	}) as typeof original;
	return () => memory;
}

async function run(config: Sample) {
	const start = performance.now();
	const disposers: (() => void)[] = [];
	let first: number | undefined;
	let memory: WebAssembly.Memory | undefined;
	let grew = false;
	let received = 0;
	const frames = config.publishers * config.subscribers * config.frames;
	// Lite-02 carries no timestamps, and a lite-05/06 subscriber drops them.
	const timed = !/^moq-lite-0[1-6]$/.test(config.version);
	const check = (bytes: Uint8Array, frame: number) => {
		first ??= performance.now() - start;
		verify(bytes, config.size, frame);
		received++;
		if (config.grow && memory && !grew) {
			const before = memory.buffer.byteLength;
			memory.grow(1);
			if (memory.buffer.byteLength !== before + 65536) throw new Error("WASM memory did not grow by one page");
			grew = true;
		}
	};
	try {
		let read: (subscriber: number) => Promise<void>;
		let startup: number;
		if (config.backend === "js") {
			// A runtime URL keeps the complete shipped backend out of the neutral harness bundle.
			const net = (await import(config.module)) as typeof Moq;
			startup = performance.now() - start;
			read = async () => {
				const origin = new net.Origin.Producer();
				const connection = await net.Connection.connect({
					url: new URL(config.url),
					consume: origin,
					websocket: { enabled: config.transport === "websocket", delay: Milli(0) },
				});
				if (connection.version !== config.version || connection.transport !== config.transport)
					throw new Error(`wrong negotiation: ${connection.version}/${connection.transport}`);
				disposers.push(() => {
					connection.abort();
					origin.close();
				});
				await Promise.all(
					Array.from({ length: config.publishers }, async (_, publisher) => {
						const request = origin.follow(net.Path.from(`${config.id}/${publisher}`));
						disposers.push(() => request.close());
						let broadcast = request.active.peek();
						while (!broadcast) {
							if (request.closed.peek() !== undefined)
								throw request.closed.peek() ?? new Error("broadcast request closed");
							await Promise.race([request.active.changed(), request.closed]);
							broadcast = request.active.peek();
						}
						const track = broadcast.track("data").subscribe();
						disposers.push(() => track.close());
						const group = await track.recvGroup();
						if (
							!group ||
							!Number.isSafeInteger(group.sequence) ||
							BigInt(group.sequence) !== BigInt(config.sequence)
						)
							throw new Error("incorrect JS group sequence");
						try {
							for (let frame = 0; frame < config.frames; frame++) {
								const got = await group.readFrame();
								if (!got || (timed ? got.timestamp?.asMillis() !== frame : got.timestamp !== undefined))
									throw new Error(`incorrect JS frame timestamp ${frame}`);
								check(got.payload, frame);
							}
							if (await group.readFrame()) throw new Error("unexpected extra JS frame");
						} finally {
							group.close();
						}
					}),
				);
			};
		} else if (config.backend === "wasm") {
			if (config.transport !== "webtransport")
				throw new Error("current WASM bindings have no WebSocket fallback");
			const wasm = (await import(config.module)) as typeof Wasm;
			const exports = await wasm.default();
			memory = exports.memory;
			wasm.setup();
			startup = performance.now() - start;
			const { url, hex } = await pin(config.url);
			const hash = Uint8Array.from(hex.match(/../g) ?? [], (byte) => Number.parseInt(byte, 16));
			read = async () => {
				const session = await wasm.Session.connectWithHashes(url.toString(), [hash]);
				if (session.version() !== config.version)
					throw new Error(`wrong WASM negotiation: ${session.version()}`);
				disposers.push(() => session.free());
				await Promise.all(
					Array.from({ length: config.publishers }, async (_, publisher) => {
						const broadcast = await session.consume(`${config.id}/${publisher}`);
						disposers.push(() => broadcast.free());
						const track = await broadcast.subscribe("data");
						disposers.push(() => track.free());
						const group = await track.recvGroup();
						if (!group || group.sequence !== BigInt(config.sequence))
							throw new Error("incorrect WASM group sequence");
						try {
							for (let frame = 0; frame < config.frames; frame++) {
								const got = await group.readFrame();
								if (!got) throw new Error(`missing WASM frame ${frame}`);
								check(got, frame);
							}
							if (await group.readFrame()) throw new Error("unexpected extra WASM frame");
						} finally {
							group.free();
						}
					}),
				);
			};
		} else {
			if (config.transport !== "webtransport") throw new Error("moq-ffi's wasm client has no WebSocket fallback");
			const memoryOf = captureMemory();
			const uniffi = (await import(config.module)) as Uniffi;
			await uniffi.uniffiInitAsync(new URL("uniffi/moq_ffi.wasm", config.module));
			memory = memoryOf();
			startup = performance.now() - start;
			const { url, hex } = await pin(config.url);
			read = async () => {
				const client = new uniffi.MoqClient(
					uniffi.MoqClientConfig.create({
						// moq-ffi reports no negotiated version, so offering only the expected one checks it.
						versions: [config.version],
						tls: uniffi.MoqClientTls.create({ fingerprints: [hex] }),
					}),
				);
				const session = await client.connect(url.toString());
				disposers.push(() => session.cancel(0));
				await Promise.all(
					Array.from({ length: config.publishers }, async (_, publisher) => {
						const broadcast = await session.consume().follow(`${config.id}/${publisher}`).available();
						const track = await broadcast.subscribeTrack("data", undefined);
						disposers.push(() => track.cancel());
						const group = await track.recvGroup();
						if (!group || group.sequence() !== BigInt(config.sequence))
							throw new Error("incorrect UniFFI group sequence");
						try {
							for (let frame = 0; frame < config.frames; frame++) {
								const got = await group.readFrame();
								if (
									!got ||
									(timed ? got.timestampUs !== BigInt(frame * 1000) : got.timestampUs !== undefined)
								)
									throw new Error(`incorrect UniFFI frame timestamp ${frame}`);
								check(new Uint8Array(got.payload), frame);
							}
							if (await group.readFrame()) throw new Error("unexpected extra UniFFI frame");
						} finally {
							group.cancel();
						}
					}),
				);
			};
		}
		const ready = performance.now();
		const memoryBefore = memory?.buffer.byteLength;
		await Promise.all(Array.from({ length: config.subscribers }, (_, subscriber) => read(subscriber)));
		if (received !== frames) throw new Error(`received ${received} frames, expected ${frames}`);
		if (config.grow && config.backend !== "js" && !grew) throw new Error("memory-growth case did not run");
		return {
			backend: config.backend,
			transport: config.transport,
			startup: { module_ms: startup, first_frame_ms: first },
			frames: { count: received, bytes: received * config.size, elapsed_ms: performance.now() - ready },
			memory: {
				wasm_before: memoryBefore ?? null,
				wasm_after: memory?.buffer.byteLength ?? null,
				forced_growth: grew,
			},
			metadata: {
				sequence: String(config.sequence),
				timestamp: !timed
					? "verified absent: this version delivers none"
					: config.backend === "wasm"
						? "unavailable: payload-only binding"
						: "verified",
			},
		};
	} finally {
		for (const dispose of disposers.reverse()) dispose();
	}
}

declare global {
	interface Window {
		baselineConsume: typeof run;
	}
}
window.baselineConsume = run;
