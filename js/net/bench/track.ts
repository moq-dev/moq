/** Sweep retained groups and subscribers for one group published through a track and received. */
import { type Consumer as GroupConsumer, Producer as GroupProducer } from "../src/group.ts";
import { Milli, Timestamp } from "../src/time.ts";
import { Producer, type Subscriber } from "../src/track.ts";

const retainedCounts = [25, 100, 400, 1500];
const subscriberCounts = [1, 4, 16];
// Open groups each subscriber is still reading, so their latency guards re-evaluate on every arrival.
const heldCounts = [0, 16];
const groups = 100;
const reps = 7;
// Publishing a group must not cost more as the retained window grows, or a guard or cache pass
// has gone back to scanning every retained group. The sweep spans 60x, so a scan lands well past
// this; the margin is loose because the machine is noisy.
const maxSlope = 5;
const payload = new Uint8Array(80);
let checksum = 0;

// A clock that advances a millisecond per group, so a window of N ms retains N groups and every
// publish ages the oldest one out. Timing reads the real clock.
const now = performance.now.bind(performance);
let clock = 0;
performance.now = () => clock;

// Let every woken reader re-evaluate its guard before the next group.
const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

function receive(subscribers: Subscriber[]): GroupConsumer[] {
	const received: GroupConsumer[] = [];
	for (const subscriber of subscribers) {
		const group = subscriber.tryRecvGroup();
		if (!group) throw new Error("group was not delivered");
		const frame = group.tryReadFrame();
		if (!frame) throw new Error("frame was not delivered");
		checksum += frame.payload.byteLength;
		received.push(group);
	}
	return received;
}

async function measure(retained: number, subscriberCount: number, held: number): Promise<number> {
	// Held groups would age out too, so those rows keep everything instead.
	const window = Milli(held > 0 ? 3_600_000 : retained);
	const producer = new Producer("bench").accept({ maxAge: window });
	const subscribers = Array.from({ length: subscriberCount }, () => producer.subscribe({ maxAge: window }));
	let sequence = 0;
	// Inserted by sequence, the way the wire hands a subscribed track its groups.
	const publish = (close: boolean) => {
		clock++;
		const group = new GroupProducer(sequence);
		producer.writeGroup(group);
		group.writeFrame({ payload, timestamp: Timestamp.fromMillis(sequence++) });
		if (close) group.close();
		return group;
	};

	// The held groups stay open, each with a reader parked on its next frame.
	const open: GroupProducer[] = [];
	const reads: Promise<unknown>[] = [];
	for (let index = 0; index < retained; index++) {
		const isHeld = index < held;
		const group = publish(!isHeld);
		const received = receive(subscribers);
		if (isHeld) {
			open.push(group);
			for (const consumer of received) reads.push(consumer.readFrame());
		}
	}
	await flush();

	const start = now();
	for (let index = 0; index < groups; index++) {
		publish(true);
		receive(subscribers);
		await flush();
	}
	const elapsed = now() - start;

	for (const group of open) group.close();
	await Promise.all(reads);
	producer.close();
	return (elapsed * 1000) / groups;
}

// Warm the JIT so the first row isn't the slowest.
await measure(100, 1, 0);

console.log("retained,subscribers,held,publish_us");
for (const held of heldCounts) {
	for (const subscriberCount of subscriberCounts) {
		let baseline: number | undefined;
		for (const retained of retainedCounts) {
			const samples: number[] = [];
			for (let rep = 0; rep < reps; rep++) samples.push(await measure(retained, subscriberCount, held));
			// The fastest run is the one least disturbed by the rest of the machine.
			const us = Math.min(...samples);
			console.log(`${retained},${subscriberCount},${held},${us.toFixed(1)}`);

			baseline ??= us;
			if (us > baseline * maxSlope) {
				throw new Error(
					`${retained} retained groups cost ${us.toFixed(1)} us/group, over ${maxSlope}x ${baseline.toFixed(1)}`,
				);
			}
		}
	}
}
if (checksum === 0) throw new Error("benchmark did no work");
