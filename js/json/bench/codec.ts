/** Sweep snapshot codec cost over document size and changed keys. */
import { Decoder } from "../src/snapshot/decoder.ts";
import { Encoder } from "../src/snapshot/encoder.ts";

const updates = 256;
console.log("keys,changed,ms_per_update");
for (const keys of [1, 10, 100, 1000]) {
	for (const changed of new Set([1, Math.min(10, keys)])) {
		{
			const samples: number[] = [];
			for (let sample = 0; sample < 3; sample++) {
				const encoder = new Encoder<Record<string, number>>({});
				const decoder = new Decoder<Record<string, number>>({});
				let value = Object.fromEntries(Array.from({ length: keys }, (_, n) => [`k${n}`, 0]));
				const start = performance.now();
				for (let n = 1; n <= updates; n++) {
					value = { ...value };
					for (let key = 0; key < changed; key++) value[`k${key}`] = n;
					const frame = encoder.update(value);
					if (frame) {
						if (frame.keyframe) decoder.snapshot(frame.payload);
						else decoder.delta(frame.payload);
						frame.commit();
					}
					if (decoder.decode()?.k0 !== n) throw new Error("codec benchmark lost an update");
				}
				samples.push((performance.now() - start) / updates);
			}
			samples.sort((a, b) => a - b);
			console.log(`${keys},${changed},${samples[1]?.toFixed(6)}`);
		}
	}
}
