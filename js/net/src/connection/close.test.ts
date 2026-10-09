import { expect, spyOn, test } from "bun:test";
import * as Ietf from "../ietf/index.ts";
import * as Lite from "../lite/index.ts";
import { createMockTransportPair, type MockTransport } from "../mock.ts";
import { Producer } from "../origin.ts";
import * as Path from "../path.ts";
import { accept } from "./accept.ts";
import { connect } from "./connect.ts";

// Hold FIN acknowledgements after delivering the bytes and FIN to the peer.
function holdFin(transport: MockTransport) {
	const reached = Promise.withResolvers<void>();
	const release = Promise.withResolvers<void>();
	let enabled = false;
	const wrap = (stream: WebTransportBidirectionalStream): WebTransportBidirectionalStream => {
		const writer = stream.writable.getWriter();
		return {
			readable: stream.readable,
			writable: new WritableStream<Uint8Array>({
				write: (bytes) => writer.write(bytes),
				abort: (reason) => writer.abort(reason),
				async close() {
					await writer.close();
					if (enabled) {
						reached.resolve();
						await release.promise;
					}
				},
			}),
		} as WebTransportBidirectionalStream;
	};
	const create = transport.createBidirectionalStream.bind(transport);
	transport.createBidirectionalStream = async (options) => wrap(await create(options));
	Object.defineProperty(transport, "incomingBidirectionalStreams", {
		value: transport.incomingBidirectionalStreams.pipeThrough(
			new TransformStream({
				transform(stream, controller) {
					controller.enqueue(wrap(stream));
				},
			}),
		),
	});
	return {
		reached: reached.promise,
		enable: () => {
			enabled = true;
		},
		release: release.resolve,
	};
}

for (const protocol of [Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_19]) {
	for (const outcome of ["acknowledged", "timeout"] as const)
		test(`graceful close withdrawal ${outcome} over ${protocol}`, async () => {
			const pair = createMockTransportPair(protocol);
			const fin = holdFin(pair.server);
			const source = new Producer();
			const destination = new Producer();
			const broadcast = source.createBroadcast(Path.from("room"));
			broadcast.announce();
			const url = new URL("https://localhost/test");
			const [client, server] = await Promise.all([
				connect({ url, transport: pair.client, consume: destination }),
				accept({ url, transport: pair.server, publish: source.consume() }),
			]);
			const announced = destination.announced();
			let timer: ReturnType<typeof spyOn<typeof globalThis, "setTimeout">> | undefined;
			try {
				expect((await announced.next())?.kind).toBe("start");
				fin.enable();
				let closed = false;
				void pair.server.closed.then(() => {
					closed = true;
				});
				let expire: (() => void) | undefined;
				if (outcome === "timeout") {
					const original = globalThis.setTimeout;
					timer = spyOn(globalThis, "setTimeout").mockImplementation(((fn: () => void, ms?: number) => {
						if (ms !== 1000) return original(fn, ms);
						expire = fn;
						return 0 as unknown as ReturnType<typeof setTimeout>;
					}) as typeof setTimeout);
				}
				const closing = server.close();
				await fin.reached;
				expect(closed).toBe(false);
				expect((await announced.next())?.kind).toBe("end");
				if (outcome === "timeout") {
					expect(expire).toBeDefined();
					expire?.();
					await expect(closing).rejects.toThrow("session close timed out");
				} else {
					fin.release();
					await closing;
				}
				await pair.server.closed;
				expect(closed).toBe(true);
			} finally {
				timer?.mockRestore();
				fin.release();
				client.abort();
				server.abort();
				announced.close();
				broadcast.close();
				source.close();
				destination.close();
			}
		});
}
