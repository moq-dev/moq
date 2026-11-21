/**
 * Sweep what a drop costs a reader with many ranges: one-group time ranges a group apart, each
 * waiting for the groups held to place it, given up lowest first so each drop places one more.
 * Mirrors the Rust `publisher_drop` bench. A drop must cost about the same however many ranges
 * the reader has, or the window has gone back to trying every range or rebuilding them all.
 */

import { StreamCode, StreamError } from "../src/error.ts";
import { hooks } from "../src/internal.ts";
import { Timescale } from "../src/time.ts";
import { Producer, type Range } from "../src/track.ts";

// `WINDOW_DROP_MAX` caps both axes, for a quick run.
const sweep = [1, 32, 1024, 8192].filter(
	(count) => count <= Number(process.env.WINDOW_DROP_MAX ?? Number.POSITIVE_INFINITY),
);
// Each cell times at least this many drops, so a cell of one drop isn't one noisy sample.
const timed = 256;

function measure(ranges: number, drops: number): number {
	const producer = new Producer("bench").accept({ timescale: Timescale.MILLI, maxAge: null });
	// Every other group, so no two ranges merge into one run, and one open range past them all so
	// the reader never settles.
	const spans: Range[] = Array.from({ length: ranges }, (_, range) => {
		const at = BigInt(2 * range);
		return { start: at, end: at };
	});
	spans.push({ start: BigInt(2 * ranges) });
	const subscriber = producer.subscribe({ ranges: spans });
	subscriber.tryRecvGroup();
	// Alternating errors keep adjacent drops separate runs.
	const errors = [new StreamError(StreamCode.NotFound), new StreamError(StreamCode.Cancel)];
	const start = performance.now();
	for (let range = 0; range < drops; range++) {
		const from = range === 0 ? 0n : BigInt(2 * range - 1);
		hooks.dropTrackGroups(producer, from, BigInt(2 * range + 1), { error: errors[range % 2] });
		subscriber.tryRecvGroup();
	}
	const elapsed = performance.now() - start;
	producer.close();
	return (elapsed * 1000) / drops;
}

// Untimed, so the first cell doesn't measure the JIT warming up.
measure(32, 32);
console.log("ranges,drops,drop_us");
for (const ranges of sweep) {
	for (const drops of sweep) {
		if (drops > ranges) continue;
		const reps = Math.max(3, Math.ceil(timed / drops));
		const times = Array.from({ length: reps }, () => measure(ranges, drops)).sort((a, b) => a - b);
		console.log(`${ranges},${drops},${times[Math.floor(reps / 2)].toFixed(2)}`);
	}
}
