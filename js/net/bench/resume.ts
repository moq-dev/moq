/**
 * Sweep readers and route swaps for groups read through a logical front, against the same
 * groups read straight from the track.
 *
 * A front reads its serving route's copy directly, so with one route it should cost about what a
 * direct read does, and each swap should leave nothing behind once its groups are read.
 */

import { Producer as BroadcastProducer } from "../src/broadcast.ts";
import { Producer as GroupProducer } from "../src/group.ts";
import { Front } from "../src/resume.ts";
import type { Subscriber } from "../src/track.ts";

const readerCounts = [1, 4, 16];
const swapCounts = [0, 1, 16];
const groups = 400;
const reps = 7;
// A reader through a front must not cost more per group as readers or swaps grow. The margin is
// loose because the machine is noisy.
const maxSlope = 3;
const payload = new Uint8Array(80);
let checksum = 0;

// Let every woken reader and pending splice run.
const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

function read(subscribers: Subscriber[]) {
	for (const subscriber of subscribers) {
		const group = subscriber.tryRecvGroup();
		if (!group) throw new Error("group was not delivered");
		const frame = group.tryReadFrame();
		if (!frame) throw new Error("frame was not delivered");
		checksum += frame.payload.byteLength;
		group.close();
		// Read until idle, as a reader parked on the next group does.
		if (subscriber.tryRecvGroup()) throw new Error("group delivered twice");
	}
}

/** Microseconds per group per reader, reading through a front after `swaps` route swaps, or directly. */
async function measure(readers: number, swaps: number | undefined): Promise<number> {
	let sequence = 0;
	const sources: { broadcast: BroadcastProducer; write: () => void }[] = [];
	const source = () => {
		const broadcast = new BroadcastProducer();
		const track = broadcast.createTrack("seq");
		// Every route's copy holds the same groups under the same sequences.
		const write = () => {
			const group = new GroupProducer(sequence);
			track.writeGroup(group);
			group.writeFrame({ payload });
			group.close();
		};
		sources.push({ broadcast, write });
		return broadcast;
	};
	const publish = () => {
		for (const { write } of sources) write();
		sequence++;
	};

	let subscribers: Subscriber[];
	let front: Front | undefined;
	if (swaps === undefined) {
		const broadcast = source().consume();
		subscribers = Array.from({ length: readers }, () => broadcast.track("seq").subscribe());
	} else {
		front = new Front();
		front.serve(source().consume());
		const consumer = front.consumer;
		subscribers = Array.from({ length: readers }, () => consumer.track("seq").subscribe());
		for (let swap = 0; swap < swaps; swap++) {
			publish();
			read(subscribers);
			// Only the serving route publishes from here, as a replaced one would stop.
			sources.splice(0, sources.length - 1);
			front.serve(source().consume());
			await flush();
		}
	}
	publish();
	read(subscribers);
	await flush();

	const start = performance.now();
	for (let index = 0; index < groups; index++) {
		publish();
		read(subscribers);
	}
	const elapsed = performance.now() - start;

	for (const subscriber of subscribers) subscriber.close();
	front?.consumer.close();
	return (elapsed * 1000) / groups / readers;
}

// Warm the JIT so the first row isn't the slowest.
await measure(4, 1);
await measure(4, undefined);

console.log("readers,swaps,direct_us,front_us");
let baseline: number | undefined;
for (const swaps of swapCounts) {
	for (const readers of readerCounts) {
		const direct: number[] = [];
		const front: number[] = [];
		// Interleaved, so a noisy stretch of the machine lands on both columns alike.
		for (let rep = 0; rep < reps; rep++) {
			direct.push(await measure(readers, undefined));
			front.push(await measure(readers, swaps));
		}
		// The fastest run is the one least disturbed by the rest of the machine.
		const directUs = Math.min(...direct);
		const frontUs = Math.min(...front);
		console.log(`${readers},${swaps},${directUs.toFixed(2)},${frontUs.toFixed(2)}`);

		baseline ??= frontUs;
		if (frontUs > baseline * maxSlope) {
			throw new Error(
				`${readers} readers after ${swaps} swaps cost ${frontUs.toFixed(2)} us/group, over ${maxSlope}x ${baseline.toFixed(2)}`,
			);
		}
	}
}
if (checksum === 0) throw new Error("benchmark did no work");
