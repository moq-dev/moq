// Measure warning traffic and catch-up time over the groups a slow consumer skips.

import { Milli } from "@moq/clock";
import { Time, Track, Varint } from "@moq/net";
import { Signal } from "@moq/signals";
import { Consumer } from "../src/container/consumer.ts";
import { Format } from "../src/container/legacy.ts";

console.log("groups,warnings,bytes,catchup_ms");
for (const count of [1, 16, 256, 1024]) {
	const track = new Track.Producer("tone");
	const delay = new Signal<Track.Delay>({ max: Milli(30_000) });
	const consumer = new Consumer(track.subscribe({ delay: { max: Milli(30_000) } }), {
		format: new Format("data"),
		delay,
	});
	const write = (sequence: bigint) => {
		const group = track.createGroup(sequence);
		group.writeFrame({ payload: Varint.encode(Number(sequence) * 1000), timestamp: Time.Timestamp.now() });
	};
	const buffered = async (timestamp: number) => {
		while (consumer.buffered.peek().at(-1)?.end !== timestamp) await consumer.buffered.changed();
	};
	let warnings = 0;
	let bytes = 0;
	const warn = console.warn;
	console.warn = (...args: unknown[]) => {
		warnings++;
		bytes += JSON.stringify(args).length;
	};
	try {
		for (let i = 0n; i < BigInt(count); i++) write(i);
		await buffered(count - 1);
		delay.set({ max: Milli.zero });
		const started = performance.now();
		write(BigInt(count));
		await buffered(count);
		console.log(`${count},${warnings},${bytes},${(performance.now() - started).toFixed(3)}`);
	} finally {
		consumer.close();
		track.close();
		console.warn = warn;
	}
}
