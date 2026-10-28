import { Micro } from "@moq/clock";
import { Terminal } from "../src/audio/terminal";

// Decoder count × callback size checks that trimming costs a span calculation,
// independent of the PCM payload size. Periodic gaps exercise the priming path.
const CALLBACKS = 4096;
// No timing suffix on warmup: the trend report would read that dimension as a measurement.
console.log("decoders,frames,warmup,ns_per_callback");
let retained = 0;
for (const count of [1, 16, 64]) {
	for (const frames of [120, 960, 2880]) {
		for (const warmup of [0, 80_000]) {
			const timings = [];
			for (let repeat = 0; repeat < 3; repeat++) {
				const decoders = Array.from({ length: count }, () => {
					const decoder = new Terminal();
					decoder.clear(0, Micro(warmup));
					return decoder;
				});
				const start = performance.now();
				for (let callback = 0; callback < CALLBACKS; callback++) {
					const timestamp = Micro((callback * frames * 1_000_000) / 48_000);
					const read = { kind: "frame" as const, group: callback, frame: { timestamp } };
					const sample = { timestamp, sampleRate: 48_000, numberOfFrames: frames };
					for (const decoder of decoders) {
						if (callback % 64 === 0) decoder.update({ kind: "gap" });
						decoder.update(read);
						retained += decoder.span(sample).frames;
					}
				}
				timings.push(((performance.now() - start) * 1e6) / (CALLBACKS * count));
			}
			timings.sort((a, b) => a - b);
			console.log(`${count},${frames},${warmup / 1000}ms,${timings[1]?.toFixed(1)}`);
		}
	}
}
if (retained === 0) throw new Error("warmup discarded the whole stream");
