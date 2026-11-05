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
					group.finish();
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
				for (const track of tracks) track.finish();
				for (const track of tracks) track.close();
			}
			rows.push({ publishers, readers, phase: "publish", us: (publish * 1000) / rounds, size_bytes: stored });
			rows.push({ publishers, readers, phase: "read", us: (read * 1000) / rounds, size_bytes: inflated });
		}
	}

	return rows;
}

/** Sweep live group tails across publishers, readers, frame sizes, and compression. */
export function tails() {
	const rows: {
		publishers: number;
		readers: number;
		frame_bytes: number;
		compressed: number;
		ns_per_frame: number;
		size_bytes: number;
	}[] = [];
	for (const publishers of [1, 4]) {
		for (const readers of [1, 4]) {
			for (const frame_bytes of [256, 1024 * 1024]) {
				for (const compress of [false, true]) {
					const frames = frame_bytes === 256 ? 9000 : 96;
					const payload = new Uint8Array(frame_bytes).fill(42);
					const tracks = Array.from({ length: publishers }, () => new Track("tail").accept({ compress }));
					const groups = tracks.map((track) => track.appendGroup());
					const consumers = groups.map((group) => Array.from({ length: readers }, () => group.consume()));
					const started = performance.now();
					for (let index = 0; index < frames; index++) {
						for (const [publisher, group] of groups.entries()) {
							group.writeFrame({ payload });
							for (const consumer of consumers[publisher]) {
								const frame = consumer.tryReadFrameSequence();
								if (
									frame?.sequence !== index ||
									frame.payload.length !== frame_bytes ||
									frame.payload[0] !== 42
								)
									throw new Error("tail reader lost a frame");
							}
						}
					}
					const ns_per_frame = ((performance.now() - started) * 1e6) / (publishers * frames);
					const size_bytes = groups.reduce((sum, group) => sum + hooks.groupCacheBytes(group), 0);
					if (size_bytes > publishers * 32 * 1024 * 1024) throw new Error("group retained its shed front");
					rows.push({
						publishers,
						readers,
						frame_bytes,
						compressed: Number(compress),
						ns_per_frame,
						size_bytes,
					});
					for (const track of tracks) track.finish();
					for (const track of tracks) track.close();
				}
			}
		}
	}
	return rows;
}
