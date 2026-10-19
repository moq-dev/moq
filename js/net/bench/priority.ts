/** Sweep subscriptions and simultaneous streams when a group or joining fill changes rank. */
import { Producer } from "../src/broadcast.ts";
import { Priority } from "../src/priority.ts";
import type { Writer } from "../src/stream.ts";

let ranked = 0;
console.log("subscriptions,streams,ns_per_change");
for (const subscriptions of [1, 16, 128]) {
	for (const streams of [4, 16, 64]) {
		const broadcast = new Producer();
		const queues = Array.from({ length: subscriptions }, (_, index) => {
			const track = broadcast.createTrack(`track-${index}`);
			const subscriber = track.subscribe();
			const priority = new Priority(subscriber);
			const writers = Array.from({ length: streams }, (_, stream) => {
				const writer = { setPriority: () => ranked++ } as unknown as Writer;
				priority.add(writer, Math.floor(stream / 2), (stream % 2) * 100);
				return writer;
			});
			return { priority, subscriber, writers };
		});
		const iterations = 100;
		const start = performance.now();
		for (let iteration = 0; iteration < iterations; iteration++) {
			for (const { priority, writers } of queues) {
				const writer = writers[streams - 1];
				priority.remove(writer);
				priority.add(writer, Math.floor((streams - 1) / 2), 100);
			}
		}
		const ns = ((performance.now() - start) * 1e6) / (iterations * subscriptions * 2);
		console.log(`${subscriptions},${streams},${ns.toFixed(1)}`);
		for (const { priority, subscriber, writers } of queues) {
			priority.close();
			for (const writer of writers) priority.remove(writer);
			subscriber.close();
		}
		broadcast.close();
	}
}
if (ranked === 0) throw new Error("benchmark did no work");
