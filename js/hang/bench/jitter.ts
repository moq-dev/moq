// Sweep the jitter estimator's held history against in-order and reordered frames, the per-frame
// cost a publisher controls through its timestamp rate.

import type { Milli } from "@moq/clock";
import { Jitter } from "../src/container/jitter.ts";

const FRAMES = 100_000;

console.log("held,order,ns_per_frame");
for (const held of [64, 512, 4096, 8192]) {
	for (const order of ["in", "reordered"] as const) {
		const jitter = new Jitter();
		// Spaced so `held` frames fit inside the two-second window.
		const step = 2000 / held;
		let media = 0;
		for (let i = 0; i < held; i++, media += step) jitter.observe(media as Milli, media as Milli);

		const started = performance.now();
		for (let i = 0; i < FRAMES; i++) {
			if (order === "in") {
				jitter.observe(media as Milli, media as Milli);
				media += step;
			} else {
				// Just behind the newest, so the reference covers almost the whole history.
				jitter.observe(media as Milli, (media - 2 * step) as Milli);
			}
		}
		const ns = ((performance.now() - started) * 1e6) / FRAMES;
		console.log(`${held},${order},${ns.toFixed(1)}`);
	}
}
