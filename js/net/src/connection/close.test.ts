import { expect, test } from "bun:test";
import { type Installed, Manual, Nano, Second } from "@moq/clock";
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

for (const protocol of [Lite.ALPN_07_WIP, Ietf.ALPN.DRAFT_18]) {
	for (const outcome of ["acknowledged", "timeout"] as const)
		test(`graceful close withdrawal ${outcome} over ${protocol}`, async () => {
			const pair = createMockTransportPair(protocol);
			const fin = holdFin(pair.server);
			const source = new Producer();
			const destination = new Producer();
			const broadcastPublication = source.createBroadcast(Path.from("room"));
			const broadcast = broadcastPublication.broadcast;
			broadcastPublication.announce();
			const url = new URL("https://localhost/test");
			const [client, server] = await Promise.all([
				connect({ url, transport: pair.client, consume: destination }),
				accept({ url, transport: pair.server, publish: source.consume() }),
			]);
			const announced = destination.announced();
			let installed: Installed | undefined;
			try {
				expect((await announced.next())?.kind).toBe("start");
				fin.enable();
				let closed = false;
				void pair.server.closed.then(() => {
					closed = true;
				});
				// Only the close's own deadline is armed on the manual clock.
				const clock = new Manual();
				if (outcome === "timeout") installed = clock.install();
				const closing = server.close();
				await fin.reached;
				expect(closed).toBe(false);
				expect((await announced.next())?.kind).toBe("end");
				if (outcome === "timeout") {
					// bun's `rejects` blocks on a pending promise, so catch first and advance.
					const timedOut = closing.catch((err: unknown) => err);
					await clock.advance(Nano.fromSecond(Second(1)));
					expect(await timedOut).toHaveProperty("message", "session close timed out");
				} else {
					fin.release();
					await closing;
				}
				await pair.server.closed;
				expect(closed).toBe(true);
			} finally {
				installed?.close();
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
