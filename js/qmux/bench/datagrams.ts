/** Sweep receive-queue draining over backlog size through the public session API. */
import * as Frame from "../src/frame.ts";
import Session from "../src/session.ts";
import type { WebSocketStreamCloseEvent } from "../src/web-socket-stream.ts";

const packet = Frame.encode({ type: "datagram", data: new Uint8Array(1200) }, "qmux-01");

async function measure(backlog: number): Promise<number> {
	let incoming!: ReadableStreamDefaultController<Uint8Array>;
	const pong = Promise.withResolvers<void>();
	const closed = Promise.withResolvers<WebSocketStreamCloseEvent>();
	const session = Session.accept(
		{
			url: "https://example.test/qmux-bench",
			opened: Promise.resolve({
				protocol: "qmux-01",
				extensions: "",
				readable: new ReadableStream<Uint8Array>({
					start: (controller) => {
						incoming = controller;
					},
				}),
				writable: new WritableStream<Uint8Array>({
					write: (bytes) => {
						if (Frame.decodeRecord(bytes).some((frame) => frame.type === "ping_response")) pong.resolve();
					},
				}),
			}),
			closed: closed.promise,
			close: () => closed.resolve({ closeCode: 1000, reason: "" }),
		},
		{ config: { idleTimeout: null } },
	);
	await session.ready;
	session.datagrams.incomingHighWaterMark = backlog;
	for (let index = 0; index < backlog; index++) incoming.enqueue(packet);
	incoming.enqueue(Frame.encode({ type: "ping_request", sequence: 0n }, "qmux-01"));
	await pong.promise;
	const reader = session.datagrams.readable.getReader();
	const start = performance.now();
	for (let index = 0; index < backlog; index++) {
		const result = await reader.read();
		if (result.done || result.value.byteLength !== 1200) throw new Error("missing datagram");
	}
	const elapsed = ((performance.now() - start) * 1000) / backlog;
	await reader.cancel();
	session.close();
	await closed.promise;
	return elapsed;
}

console.log("backlog,us_per_read");
for (const backlog of [1, 32, 1024, 16384]) {
	const samples: number[] = [];
	for (let run = 0; run < 7; run++) samples.push(await measure(backlog));
	samples.sort((a, b) => a - b);
	console.log(`${backlog},${samples[3].toFixed(3)}`);
}
