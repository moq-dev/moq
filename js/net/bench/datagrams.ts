/** Sweep datagram tracks x readers, with a stalled reader sharing each bounded buffer. */
import { Producer } from "../src/datagram.ts";

const payload = new Uint8Array(1024);
async function measure(tracks: number, readers: number): Promise<{ join: number; delivery: number }> {
	const joining = performance.now();
	const producers = Array.from({ length: tracks }, () => new Producer("bench").accept());
	const consumers = producers.map((producer) => Array.from({ length: readers }, () => producer.consume()));
	const stalled = producers.map((producer) => producer.consume());
	const join = ((performance.now() - joining) * 1e6) / (tracks * (readers + 1));
	const start = performance.now();
	for (let i = 0; i < 256; i++) {
		for (const [index, producer] of producers.entries()) {
			producer.append(undefined, payload);
			for (const consumer of consumers[index]) {
				if ((await consumer.recv())?.sequence !== BigInt(i)) throw new Error("missing datagram");
			}
		}
	}
	const elapsed = performance.now() - start;
	for (const producer of producers) producer.finish();
	for (const row of consumers) for (const consumer of row) consumer.close();
	for (const consumer of stalled) consumer.close();
	return { join, delivery: (elapsed * 1e6) / (tracks * readers * 256) };
}
await measure(4, 4);
console.log("tracks,readers,ns_per_join,ns_per_delivery");
for (const tracks of [1, 16, 64])
	for (const readers of [1, 16, 64]) {
		const runs = [];
		for (let run = 0; run < 3; run++) runs.push(await measure(tracks, readers));
		runs.sort((a, b) => a.delivery - b.delivery);
		console.log(`${tracks},${readers},${runs[1].join.toFixed(1)},${runs[1].delivery.toFixed(1)}`);
	}
