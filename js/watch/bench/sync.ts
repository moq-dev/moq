/** Sweep clock-reset work over independent players and parked frames on one player. */
import { Time } from "@moq/net";
import { Sync } from "../src/sync.ts";

console.log("players,parked_frames,arm_us,reset_us");
for (const players of [1, 16, 128]) {
	for (const frames of [1, 16, 128]) {
		const clocks = Array.from({ length: players }, () => new Sync({ delay: Time.Milli(60_000) }));
		// Let the fixed delay propagate without advancing media time.
		for (let i = 0; i < 10; i++) await Promise.resolve();
		let armed = 0;
		let reset = 0;
		try {
			for (let iteration = -20; iteration < 100; iteration++) {
				const clock = clocks[0];
				const timestamp = Time.Milli.now();
				clock.received(timestamp);
				const start = performance.now();
				const waiting = Array.from({ length: frames }, () => clock.wait(timestamp));
				const ready = performance.now();
				clock.reset();
				const valid = await Promise.all(waiting);
				const done = performance.now();
				if (valid.some((presentable) => presentable !== false))
					throw new Error("reset left a frame presentable");
				if (iteration >= 0) {
					armed += ready - start;
					reset += done - ready;
				}
			}
			console.log(
				`${players},${frames},${((armed * 1000) / 100).toFixed(1)},${((reset * 1000) / 100).toFixed(1)}`,
			);
		} finally {
			for (const clock of clocks) clock.close();
		}
	}
}
