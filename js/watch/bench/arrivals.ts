import { Milli } from "@moq/clock";
import { Producer } from "../src/arrivals";

// Sweep open handles against each handle's unread backlog: a write should cost one push per
// handle, never a slope in the backlog.
const frame = { kind: "frame" as const, timestamp: Milli(0), arrival: Milli(0), late: false };
const WRITES = 10_000;

console.log("handles,backlog,ns_per_handle_write");
for (const count of [1, 4, 16]) {
	for (const backlog of [0, 256, 4096]) {
		const producer = new Producer();
		const handles = Array.from({ length: count }, () => producer.consume());
		for (let i = 0; i < backlog; i++) producer.write(frame);

		const start = performance.now();
		for (let n = 0; n < WRITES; n++) {
			producer.write(frame);
			for (const handle of handles) await handle.next();
		}
		const ns = ((performance.now() - start) * 1e6) / (WRITES * count);

		for (const handle of handles) {
			for (let i = 0; i < backlog; i++) if ((await handle.next()) === undefined) throw new Error("lost arrival");
			handle.close();
		}
		console.log(`${count},${backlog},${ns.toFixed(0)}`);
	}
}
