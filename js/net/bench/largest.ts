/** Sweep tracks and subscribers parked on `largest()` for the cost of finishing one-frame groups. */

import { Producer as GroupProducer } from "../src/group.ts";
import { Producer, type Subscriber } from "../src/track.ts";

const trackCounts = [1, 16];
const subscriberCounts = [1, 16, 256];
const groups = 50;
const reps = 7;
// Finishing a group already costs a pass over its subscribers' mirrors. Unbounded readers
// parked on `largest()` must not add another: only a reader capped below the largest follows
// a finish that leaves the largest in place. Waking every parked reader on each finish costs
// twice as much with one subscriber and nine times at 256; the margin is loose for a noisy machine.
const maxOverhead = 4;
const payload = new Uint8Array(80);

type Readers = "none" | "unbounded" | "capped";

// Let every woken reader re-evaluate before the next step.
const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

// Follow `largest()` until the track ends, the way a live reader parks on it.
async function follow(subscriber: Subscriber): Promise<void> {
	while ((await subscriber.largest()) !== undefined) {}
}

async function measure(trackCount: number, subscriberCount: number, readers: Readers): Promise<number> {
	const producers = Array.from({ length: trackCount }, () => new Producer("bench").accept({}));
	const following: Promise<void>[] = [];
	const subscribers: Subscriber[] = [];
	for (const producer of producers) {
		for (let index = 0; index < subscriberCount; index++) {
			const subscriber = producer.subscribe();
			// Capped readers stop at group 1, below every group the sweep finishes.
			if (readers === "capped") subscriber.setGroups({ end: { excluded: 1 } });
			if (readers !== "none") following.push(follow(subscriber));
			subscribers.push(subscriber);
		}
	}
	// Group 0 is under every cap, so a capped reader has something to report and follow.
	for (const producer of producers) {
		const group = new GroupProducer(0);
		producer.writeGroup(group);
		group.writeFrame({ payload });
		group.close();
	}
	await flush();

	let elapsed = 0;
	for (let sequence = 1; sequence <= groups; sequence++) {
		const open = producers.map((producer) => {
			const group = new GroupProducer(sequence);
			producer.writeGroup(group);
			group.writeFrame({ payload });
			return group;
		});
		await flush();

		const start = performance.now();
		for (const group of open) group.close();
		await flush();
		elapsed += performance.now() - start;
	}

	for (const producer of producers) producer.close();
	await Promise.all(following);
	for (const subscriber of subscribers) subscriber.close();
	return (elapsed * 1000) / (groups * trackCount);
}

// The fastest run is the one least disturbed by the rest of the machine.
async function fastest(trackCount: number, subscriberCount: number, readers: Readers): Promise<number> {
	const samples: number[] = [];
	for (let rep = 0; rep < reps; rep++) samples.push(await measure(trackCount, subscriberCount, readers));
	return Math.min(...samples);
}

// Warm the JIT so the first row isn't the slowest.
for (const readers of ["none", "unbounded", "capped"] as const) await measure(1, 16, readers);

console.log("tracks,subscribers,readers,finish_us");
for (const trackCount of trackCounts) {
	for (const subscriberCount of subscriberCounts) {
		const rows = {} as Record<Readers, number>;
		for (const readers of ["none", "unbounded", "capped"] as const) {
			rows[readers] = await fastest(trackCount, subscriberCount, readers);
			console.log(`${trackCount},${subscriberCount},${readers},${rows[readers].toFixed(2)}`);
		}
		// Capped readers follow every finish below the edge, so only unbounded readers must be free.
		if (rows.unbounded > rows.none * maxOverhead) {
			throw new Error(
				`${subscriberCount} unbounded readers cost ${rows.unbounded.toFixed(2)} us per finish, over ${maxOverhead}x ${rows.none.toFixed(2)} without them`,
			);
		}
	}
}
