/**
 * Sweep readers and route swaps for direct and forked readers through a logical front,
 * against the same groups read straight from the track.
 *
 * A front reads its serving route's copy directly, so with one route it should cost about what a
 * direct read does, and each swap should leave nothing behind once its groups are read.
 */

import { Milli } from "@moq/clock";
import { Producer as BroadcastProducer } from "../src/broadcast.ts";
import { Producer as GroupProducer } from "../src/group.ts";
import { hooks } from "../src/internal.ts";
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
async function measure(readers: number, swaps: number | undefined, fork = false): Promise<number> {
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
			group.finish();
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
		const track = consumer.track("seq");
		if (fork) {
			const parent = track.subscribe();
			subscribers = Array.from({ length: readers }, () => parent.fork());
			parent.close();
		} else {
			subscribers = Array.from({ length: readers }, () => track.subscribe());
		}
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
await measure(4, 1, true);
await measure(4, undefined);

console.log("readers,swaps,direct_us,front_us,fork_us");
let baseline: number | undefined;
for (const swaps of swapCounts) {
	for (const readers of readerCounts) {
		const direct: number[] = [];
		const front: number[] = [];
		const fork: number[] = [];
		// Interleaved, so a noisy stretch of the machine lands on both columns alike.
		for (let rep = 0; rep < reps; rep++) {
			direct.push(await measure(readers, undefined));
			front.push(await measure(readers, swaps));
			fork.push(await measure(readers, swaps, true));
		}
		// The fastest run is the one least disturbed by the rest of the machine.
		const directUs = Math.min(...direct);
		const frontUs = Math.min(...front);
		const forkUs = Math.min(...fork);
		console.log(`${readers},${swaps},${directUs.toFixed(2)},${frontUs.toFixed(2)},${forkUs.toFixed(2)}`);

		baseline ??= frontUs;
		if (Math.max(frontUs, forkUs) > baseline * maxSlope) {
			throw new Error(
				`${readers} readers after ${swaps} swaps cost ${Math.max(frontUs, forkUs).toFixed(2)} us/group, over ${maxSlope}x ${baseline.toFixed(2)}`,
			);
		}
	}
}
if (checksum === 0) throw new Error("benchmark did no work");

// Two routes keep the same live group while taking turns. Sweep audience and flap
// count: the cost per swap must not grow with redundant subscriptions to those routes.
async function measureFlaps(readers: number, flaps: number): Promise<number> {
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const tracks = sources.map((source) => source.createTrack("seq"));
	const groups = tracks.map((track) => {
		const group = track.appendGroup();
		group.writeFrame({ payload });
		return group;
	});
	front.serve(handles[0]);
	const subscribers = Array.from({ length: readers }, () =>
		front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) }),
	);
	const held = subscribers.map((subscriber) => {
		const group = subscriber.tryRecvGroup();
		if (!group) throw new Error("missing held group");
		return group;
	});
	await flush();
	const start = performance.now();
	for (let flap = 0; flap < flaps; flap++) {
		front.serve(handles[(flap + 1) % 2]);
		await flush();
		for (const subscriber of subscribers) {
			if (subscriber.tryRecvGroup()) throw new Error("repeated held group");
		}
	}
	const elapsed = performance.now() - start;
	for (const group of held) group.close();
	for (const subscriber of subscribers) subscriber.close();
	front.consumer.close();
	for (const group of groups) group.finish();
	for (const handle of handles) handle.close();
	for (const source of sources) source.close();
	await flush();
	return (elapsed * 1000) / readers / flaps;
}

await measureFlaps(4, 16);
console.log("readers,flaps,flap_us");
for (const readers of readerCounts) {
	for (const flaps of [1, 16, 64]) {
		const samples: number[] = [];
		for (let rep = 0; rep < reps; rep++) samples.push(await measureFlaps(readers, flaps));
		console.log(`${readers},${flaps},${Math.min(...samples).toFixed(2)}`);
	}
}

// An adopted copy may repeat a large prefix before it reaches unread content. Time readiness
// separately from constructing the copies, sweeping both the replay and independent readers.
async function measureReplay(readers: number, replayed: number): Promise<number> {
	const producer = new GroupProducer(0);
	const replacement = new GroupProducer(0);
	for (let index = 0; index < replayed; index++) {
		producer.writeFrame({ payload });
		replacement.writeFrame({ payload });
	}
	replacement.writeFrame({ payload });
	const consumers = Array.from({ length: readers }, () => producer.mirror());
	for (const consumer of consumers) {
		for (let index = 0; index < replayed; index++) {
			if (!consumer.tryReadFrame()) throw new Error("missing original frame");
		}
		const copy = replacement.mirror();
		hooks.recoverGroup(consumer, {
			changed: producer.closed,
			wants: (failed) => failed !== undefined,
			replace: async () => ({
				group: copy,
				expiry: { expired: () => false, changed: [] },
				adopt: () => {},
				discard: () => {},
			}),
		});
	}
	producer.abort(new Error("route failed"));
	const start = performance.now();
	await Promise.all(consumers.map((consumer) => consumer.readable()));
	const elapsed = performance.now() - start;
	for (const consumer of consumers) {
		const frame = consumer.tryReadFrameSequence();
		if (frame?.sequence !== replayed) throw new Error("readiness did not preserve the unread frame");
		checksum += frame.payload.byteLength;
		consumer.close();
	}
	replacement.finish();
	await flush();
	return (elapsed * 1000) / readers;
}

await measureReplay(4, 64);
console.log("readers,replayed_frames,readable_us");
for (const replayed of [0, 1, 64, 1024]) {
	for (const readers of readerCounts) {
		const samples: number[] = [];
		for (let rep = 0; rep < reps; rep++) samples.push(await measureReplay(readers, replayed));
		console.log(`${readers},${replayed},${Math.min(...samples).toFixed(2)}`);
	}
}

// Sweep audience and delivered history: a read must touch only the delivered identity,
// whether the history is under, at, or past the replay-buffer cap on retained identities.
async function measureDatagrams(readers: number, retained: number): Promise<number> {
	const front = new Front();
	const broadcast = new BroadcastProducer();
	const producer = broadcast.createTrack("seq");
	front.serve(broadcast.consume());
	const subscribers = Array.from({ length: readers }, () =>
		front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) }),
	);
	const deliver = (sequence: number) => {
		producer.insertDatagram(sequence, undefined, payload);
		for (const subscriber of subscribers) {
			const recv = hooks.tryRecvDatagram(subscriber);
			if (recv.kind !== "datagram") throw new Error("datagram was not delivered");
			checksum += recv.datagram.sequence;
		}
	};
	for (let sequence = 0; sequence < retained; sequence++) deliver(sequence);
	const start = performance.now();
	for (let index = 0; index < groups; index++) deliver(retained + index);
	const elapsed = performance.now() - start;
	for (const subscriber of subscribers) subscriber.close();
	front.consumer.close();
	broadcast.close();
	await flush();
	return (elapsed * 1000) / groups / readers;
}

await measureDatagrams(4, 64);
console.log("datagram_readers,retained,us_per_datagram");
for (const retained of [1, 64, 4096]) {
	for (const readers of readerCounts) {
		const runs: number[] = [];
		for (let rep = 0; rep < reps; rep++) runs.push(await measureDatagrams(readers, retained));
		console.log(`${readers},${retained},${Math.min(...runs).toFixed(2)}`);
	}
}
