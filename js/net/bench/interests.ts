/** Sweep broadcast and track counts for acquiring and releasing a paired interest. */
import { Interests, Slots } from "../src/util/limits.ts";

const counts = [1, 16, 256];
const reps = 7;
const operations = 10_000;
// Charging one track must not cost more as the session holds more broadcasts and tracks, or the
// accounting has gone back to scanning them. The sweep spans 65,536x, so a scan lands well past
// this; the margin is loose because the machine is noisy.
const maxSlope = 5;

let baseline: number | undefined;
for (const broadcasts of counts) {
	for (const tracks of counts) {
		const interests = new Interests(new Slots(broadcasts * tracks + 1, { reason: "bench" }));
		const held = [];
		for (let broadcast = 0; broadcast < broadcasts; broadcast++) {
			for (let track = 0; track < tracks; track++)
				held.push(interests.acquire(`${broadcast}`, `${track}`, "track"));
		}
		const times = [];
		for (let rep = 0; rep < reps; rep++) {
			const start = performance.now();
			// The SUBSCRIBE pairs with a held TRACK. A fresh track is left out: inserting and
			// deleting one key costs O(size) near a hash table's capacity in both V8 and JSC, which
			// would measure the engine rather than the accounting.
			for (let operation = 0; operation < operations; operation++) {
				interests.acquire("0", "0", "subscribe").release();
			}
			times.push(((performance.now() - start) * 1000) / operations);
		}
		times.sort((a, b) => a - b);
		const us = times[Math.floor(reps / 2)] ?? 0;
		console.log(`broadcasts=${broadcasts} tracks=${tracks} acquire+release=${us.toFixed(3)} us`);
		baseline ??= us;
		if (us > baseline * maxSlope) {
			throw new Error(
				`${broadcasts}x${tracks} held interests cost ${us.toFixed(3)} us, over ${maxSlope}x ${baseline.toFixed(3)}`,
			);
		}
		for (const interest of held) interest.release();
	}
}
