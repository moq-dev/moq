/** Sweep buffered JSON stream reads over record count, payload size, and track compression. */
import { Track } from "@moq/net";
import { Consumer, Producer } from "../src/stream/index.ts";

console.log("records,payload_bytes,compress,ns_per_record");
for (const records of [64, 1024]) {
	for (const bytes of [16, 1024]) {
		for (const compress of [false, true]) {
			const samples: number[] = [];
			for (let sample = 0; sample < 6; sample++) {
				const track = new Track.Producer("stream-bench").accept({ compress });
				const producer = new Producer<{ n: number; text: string }>({ track });
				const text = "x".repeat(bytes);
				for (let n = 0; n < records; n++) producer.append({ value: { n, text } });
				producer.finish();

				const consumer = new Consumer<{ n: number; text: string }>({ track: track.subscribe() });
				const start = performance.now();
				for (let n = 0; n < records; n++) {
					const record = await consumer.next();
					if (record?.value.n !== n || record.value.text !== text || record.at !== undefined) {
						throw new Error("stream benchmark lost a record");
					}
				}
				if ((await consumer.next()) !== undefined) throw new Error("stream benchmark did not finish");
				if (sample > 0) samples.push(((performance.now() - start) * 1e6) / records);
			}
			samples.sort((a, b) => a - b);
			console.log(`${records},${bytes},${compress},${samples[2]?.toFixed(1)}`);
		}
	}
}
