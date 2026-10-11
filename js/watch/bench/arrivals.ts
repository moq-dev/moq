import { Time } from "@moq/net";
import { Signal } from "@moq/signals";
import { type Arrival, recordArrival } from "../src/arrivals";

console.log("decoders,retained_frames,ns_per_arrival");
for (const count of [1, 4, 16]) {
	for (const retained of [1, 256, 4096]) {
		const initial: Arrival[] = Array.from({ length: retained }, () => ({
			timestamp: Time.Milli(0),
			arrival: Time.Milli(0),
			late: false,
			skipped: false,
		}));
		const windows = Array.from({ length: count }, () => new Signal<readonly Arrival[]>(initial));
		const frame = initial[0];
		const start = performance.now();
		for (let n = 0; n < 1000; n++)
			for (const window of windows) {
				window.set(initial);
				recordArrival(window, frame);
			}
		const ns = ((performance.now() - start) * 1e6) / (1000 * count);
		for (const window of windows)
			if (window.peek().length !== Math.min(retained + 1, 4096)) throw new Error("lost arrival");
		console.log(`${count},${retained},${ns.toFixed(0)}`);
	}
}
