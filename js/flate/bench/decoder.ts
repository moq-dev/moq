/** Sweep retained compressed stream frames and oversized slices; timings are informational. */
import { Group, Track } from "@moq/net";
import { Encoder } from "../src/codec.ts";
import { Consumer, Producer } from "../src/stream/index.ts";

console.log("case,frames,payload_bytes,retained_bytes,ms");
for (const frames of [64, 1024]) {
	for (const bytes of [1, 1024]) {
		const track = new Track.Producer("retained-bench").accept({});
		const producer = new Producer({ track, compression: "deflate" });
		for (let n = 0; n < frames; n++) producer.append({ value: new Uint8Array(bytes).fill(n) });
		producer.finish();
		const samples: number[] = [];
		let retained = 0;
		for (let sample = 0; sample < 4; sample++) {
			const subscription = track.subscribe();
			try {
				const consumer = new Consumer({ track: subscription, compression: "deflate" });
				const values: Uint8Array[] = [];
				const start = performance.now();
				for await (const { value } of consumer) values.push(value);
				const elapsed = performance.now() - start;
				if (values.length !== frames || values.some((v, n) => v.length !== bytes || v[0] !== (n & 0xff)))
					throw new Error("stream benchmark lost a frame");
				retained = values.reduce((sum, value) => sum + value.buffer.byteLength, 0);
				if (sample > 0) samples.push(elapsed);
			} finally {
				subscription.close();
			}
		}
		samples.sort((a, b) => a - b);
		console.log(`retained,${frames},${bytes},${retained},${samples[1]?.toFixed(3)}`);
	}
}

for (const excess of [64 * 1024, 1024 * 1024, 8 * 1024 * 1024]) {
	const bytes = Group.MAX_GROUP_CACHE_BYTES + excess;
	const slice = new Encoder().frame(new Uint8Array(bytes));
	// Write the hostile slice directly: a conforming producer refuses its inflated size.
	const track = new Track.Producer("overflow-bench").accept({});
	const group = track.appendGroup();
	group.writeFrame({ payload: slice });
	group.close();
	track.close();
	const samples: number[] = [];
	for (let sample = 0; sample < 4; sample++) {
		const subscription = track.subscribe();
		const consumer = new Consumer({ track: subscription, compression: "deflate" });
		const start = performance.now();
		try {
			await consumer.next();
			throw new Error("stream benchmark accepted an oversized slice");
		} catch (error) {
			if (!(error instanceof Error) || !error.message.includes("decompressed group exceeded")) throw error;
			if (sample > 0) samples.push(performance.now() - start);
		} finally {
			subscription.close();
		}
	}
	samples.sort((a, b) => a - b);
	console.log(`overflow,1,${bytes},0,${samples[1]?.toFixed(3)}`);
}
