import type { Producer as Group } from "../src/group.ts";
import { hooks } from "../src/internal.ts";
import { Producer as Track } from "../src/track.ts";

/** Sweep compressed publishers and public group readers, sharing one inflated copy per group. */
export function fanout() {
	const payload = new TextEncoder().encode('{"value":123,"status":"unchanged"}\n'.repeat(64));
	const frames = 32;
	const rounds = 10;
	const rows: { publishers: number; readers: number; phase: string; us: number; size_bytes: number }[] = [];
	for (const publishers of [1, 4, 16]) {
		for (const readers of [1, 4, 16]) {
			let publish = 0;
			let read = 0;
			let stored = 0;
			let inflated = 0;
			for (let round = 0; round < rounds; round++) {
				const tracks = Array.from({ length: publishers }, () => new Track("bench").accept({ compress: true }));
				const groups: Group[] = tracks.map((track) => track.appendGroup());
				const consumers = groups.map((group) => Array.from({ length: readers }, () => group.consume()));
				let started = performance.now();
				for (const group of groups) {
					for (let index = 0; index < frames; index++) group.writeFrame({ payload });
					group.close();
				}
				publish += performance.now() - started;
				stored = groups.reduce((bytes, group) => bytes + hooks.groupCacheBytes(group), 0);
				started = performance.now();
				for (const subscribers of consumers) {
					let shared: Uint8Array | undefined;
					for (const consumer of subscribers) {
						for (let index = 0; index < frames; index++) {
							const frame = consumer.tryReadFrame();
							if (!frame || frame.payload.length !== payload.length) throw new Error("missing payload");
							if (index === 0) {
								if (shared && shared !== frame.payload) throw new Error("fanout duplicated inflation");
								shared = frame.payload;
							}
						}
						consumer.close();
					}
				}
				read += performance.now() - started;
				inflated = groups.reduce((bytes, group) => bytes + hooks.groupCacheBytes(group), 0) - stored;
				if (inflated !== publishers * frames * payload.length)
					throw new Error("inflated bytes scale with readers");
				for (const track of tracks) track.close();
			}
			rows.push({ publishers, readers, phase: "publish", us: (publish * 1000) / rounds, size_bytes: stored });
			rows.push({ publishers, readers, phase: "read", us: (read * 1000) / rounds, size_bytes: inflated });
		}
	}

	return rows;
}
