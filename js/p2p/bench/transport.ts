/** Sweep concurrent streams and bytes per stream through the real qmux adapter. @module */
import { peers } from "../src/mock.ts";
import { Transport } from "../src/transport.ts";

async function receive(stream: ReadableStream<Uint8Array>) {
	const reader = stream.getReader();
	let bytes = 0;
	try {
		for (;;) {
			const { value, done } = await reader.read();
			if (done) return bytes;
			bytes += value.byteLength;
		}
	} finally {
		reader.releaseLock();
	}
}

async function measure(streams: number, bytes: number) {
	const { left, right } = peers();
	const protocol = "moq-lite-07-wip";
	const client = Transport.connect({ peer: left.asPeer(), protocol });
	const server = Transport.accept({ peer: right.asPeer(), channel: right.incoming.asChannel(), protocol });
	left.channel.open();
	right.incoming.open();
	await Promise.all([client.ready, server.ready]);
	const reader = server.incomingUnidirectionalStreams.getReader();
	const receiving = (async () => {
		const pending: Promise<number>[] = [];
		for (let i = 0; i < streams; i++) {
			const { value, done } = await reader.read();
			if (done) throw new Error("missing benchmark stream");
			pending.push(receive(value));
		}
		return (await Promise.all(pending)).reduce((total, value) => total + value, 0);
	})();
	const payload = new Uint8Array(bytes);
	const start = performance.now();
	await Promise.all(
		Array.from({ length: streams }, async () => {
			const writer = (await client.createUnidirectionalStream()).getWriter();
			await writer.write(payload);
			await writer.close();
		}),
	);
	const delivered = await receiving;
	const elapsed = performance.now() - start;
	if (delivered !== streams * bytes || left.channel.records.some((record) => record.byteLength > 16384))
		throw new Error("benchmark delivery changed");
	console.log(`${streams},${bytes},${left.channel.records.length},${elapsed.toFixed(3)}`);
	reader.releaseLock();
	client.close();
	await server.closed;
}

console.log("streams,bytes_per_stream,records,elapsed_ms");
for (const streams of [1, 8, 64]) for (const bytes of [1024, 16384, 262144]) await measure(streams, bytes);
