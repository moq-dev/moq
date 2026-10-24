/** Sweep compressed publishers x plain readers, reporting one shared inflated copy per group, then compressed datagram retention. */
import { gzipSync } from "node:zlib";
import { Encoder } from "../src/flate/encoder.ts";
import type { Producer as Group } from "../src/group.ts";
import { hooks } from "../src/internal.ts";
import { Producer as Track } from "../src/track.ts";

const payload = new TextEncoder().encode('{"value":123,"status":"unchanged"}\n'.repeat(64));
const frames = 32;
const rounds = 10;
console.log("publishers,readers,phase,us,size_bytes");
for (const publishers of [1, 4, 16]) {
	for (const readers of [1, 4, 16]) {
		let publish = 0;
		let read = 0;
		let stored = 0;
		let inflated = 0;
		for (let round = 0; round < rounds; round++) {
			const tracks = Array.from({ length: publishers }, () => new Track("bench").accept({ compress: true }));
			const groups: Group[] = tracks.map((track) => track.appendGroup());
			const consumers = groups.map((group) => Array.from({ length: readers }, () => group.mirror()));
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
			if (inflated !== publishers * frames * payload.length) throw new Error("inflated bytes scale with readers");
			for (const track of tracks) track.close();
		}
		console.log([publishers, readers, "publish", ((publish * 1000) / rounds).toFixed(1), stored].join(","));
		console.log([publishers, readers, "read", ((read * 1000) / rounds).toFixed(1), inflated].join(","));
	}
}

// Received compressed datagrams of zeros, each inflating to the largest payload, swept over
// tracks x queued datagrams. A fast reader takes each one while a slow reader never reads, so
// the heap the slow queues retain shows whether they hold the slices or inflated copies.
const zeros = new Uint8Array(65535);
const slice = new Encoder().frame(zeros);
async function datagrams(count: number, queued: number): Promise<[number, number]> {
	Bun.gc(true);
	const before = process.memoryUsage().heapUsed;
	const tracks = Array.from({ length: count }, () => new Track("bench").accept({ compress: true }));
	const slow = tracks.map((track) => track.subscribe());
	const fast = tracks.map((track) => track.subscribe());
	const started = performance.now();
	for (let sequence = 0; sequence < queued; sequence++) {
		for (const [index, track] of tracks.entries()) {
			// Its own copy, as each received datagram carries its own wire bytes.
			hooks.writeDatagram(track, { sequence, payload: slice.slice() }, zeros.length);
			const datagram = await fast[index].recvDatagram();
			if (datagram?.payload.length !== zeros.length) throw new Error("missing payload");
		}
	}
	const deliver = ((performance.now() - started) * 1000) / (count * queued);
	Bun.gc(true);
	const retained = process.memoryUsage().heapUsed - before;
	for (const track of tracks) track.close();
	slow.length = 0;
	return [deliver, retained];
}
await datagrams(16, 64);
console.log("tracks,queued,deliver_us,retained_bytes");
for (const count of [1, 16, 64]) {
	for (const queued of [1, 8, 64]) {
		const [deliver, retained] = await datagrams(count, queued);
		console.log([count, queued, deliver.toFixed(1), retained].join(","));
	}
}

// This entry is an application importing only its track consumer. Synchronous model writes
// share the module, so its bundle includes the encoder as well as the decoder.
const bundle = await Bun.build({
	entrypoints: [new URL("./fixtures/compression-consumer.ts", import.meta.url).pathname],
	minify: true,
	target: "browser",
});
if (!bundle.success) throw new AggregateError(bundle.logs, "consumer bundle failed");
const bytes = new Uint8Array(await bundle.outputs[0].arrayBuffer());
console.log("measurement,size_bytes");
console.log(`consumer_bundle,${bytes.length}`);
console.log(`consumer_bundle_gzip,${gzipSync(bytes).length}`);
