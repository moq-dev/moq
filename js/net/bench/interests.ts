/** Sweep broadcast and track counts for acquiring and releasing a paired interest. */
import { Interests, Slots } from "../src/util/limits.ts";

const rounds = 7;
const operations = 10_000;
for (const broadcasts of [1, 16, 256]) {
	for (const tracks of [1, 16, 256]) {
		const interests = new Interests(new Slots(broadcasts * tracks + 1));
		const held = [];
		for (let path = 0; path < broadcasts; path++) {
			for (let track = 0; track < tracks; track++) held.push(interests.acquire(`${path}`, `${track}`, "track"));
		}
		const times = [];
		for (let round = 0; round < rounds; round++) {
			const start = performance.now();
			for (let operation = 0; operation < operations; operation++) {
				interests.acquire("0", "0", "subscribe").release();
			}
			times.push(((performance.now() - start) * 1000) / operations);
		}
		times.sort((a, b) => a - b);
		console.log(
			`broadcasts=${broadcasts} tracks=${tracks} acquire+release=${times[Math.floor(rounds / 2)]?.toFixed(3)} us`,
		);
		for (const interest of held) interest.release();
	}
}
