// Run with `bun js/hang/bench/slow-group.ts` to measure warning traffic per catch-up.
import { Group, Time, Track, Varint } from "@moq/net";
import { Signal } from "@moq/signals";
import { Consumer } from "../src/container/consumer.ts";
import { Format } from "../src/container/legacy.ts";

for (const count of [1, 16, 256, 1024]) {
	const track = new Track.Producer("tone");
	const maxAge = new Signal(Time.Milli(30_000));
	const consumer = new Consumer(track.subscribe({ maxAge: Time.Milli(30_000) }), {
		format: new Format("data"),
		maxAge,
	});
	const write = (sequence: number) => {
		const group = new Group.Producer(sequence);
		group.writeFrame({ payload: Varint.encode(sequence * 1000), timestamp: Time.Timestamp.now() });
		track.writeGroup(group);
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
		for (let i = 0; i < count; i++) write(i);
		await buffered(count - 1);
		maxAge.set(Time.Milli.zero);
		const started = performance.now();
		write(count);
		await buffered(count);
		console.log(JSON.stringify({ groups: count, warnings, bytes, catchupMs: performance.now() - started }));
	} finally {
		consumer.close();
		track.close();
		console.warn = warn;
	}
}
