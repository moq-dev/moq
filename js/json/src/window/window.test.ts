import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { type Event, Producer } from "./index.ts";

type Rec = { n: number };

/**
 * A producer and a consumer that reads after every edit.
 *
 * Polling as the publisher goes is what "keeping up" means: a consumer left until the end is a whole
 * group behind, and the default subscription abandons a group as soon as a newer one exists, so it
 * would resume at the newest reset instead of reading the rolls in between.
 */
class Live {
	producer: Producer<Rec>;
	consumer: Consumer<Rec>;
	events: Event<Rec>[] = [];

	constructor(config: { opRatio?: number; compression?: boolean } = {}) {
		const track = new Track.Producer("test");
		this.producer = new Producer<Rec>(track, config);
		this.consumer = new Consumer<Rec>(track.subscribe(), { compression: config.compression });
	}

	async push(n: number): Promise<void> {
		this.producer.push({ n });
		await this.read();
	}

	async pop(count: number): Promise<void> {
		this.producer.pop(count);
		await this.read();
	}

	// Drain whatever is decodable right now. `next()` blocks once the queue is empty, so race it
	// against a turn of the event loop rather than awaiting it directly.
	async read(): Promise<void> {
		for (;;) {
			const idle = Symbol("idle");
			const event = await Promise.race([
				this.consumer.next(),
				new Promise<typeof idle>((resolve) => setTimeout(() => resolve(idle), 0)),
			]);
			if (event === idle || event === undefined) return;
			this.events.push(event);
		}
	}

	async finish(): Promise<Event<Rec>[]> {
		this.producer.finish();
		await this.read();
		return this.events;
	}
}

const pushed = (events: Event<Rec>[]): number[] => events.flatMap((e) => ("push" in e ? [e.push.index] : []));

test("push and pop round-trip", async () => {
	const live = new Live();
	await live.push(0);
	await live.push(1);
	await live.pop(1);
	await live.push(2);

	expect(await live.finish()).toEqual([
		{ push: { index: 0, value: { n: 0 } } },
		{ push: { index: 1, value: { n: 1 } } },
		{ pop: 0 },
		{ push: { index: 2, value: { n: 2 } } },
	]);
});

test("a popped record is never restated", async () => {
	// Ops disabled, so every single edit is its own reset restating the whole window.
	const live = new Live({ opRatio: 0 });
	await live.push(0);
	await live.push(1);
	await live.pop(1);
	await live.push(2);

	// Every edit restates the window, yet a record already delivered is never pushed twice. That is
	// the property an append-only log cannot provide.
	expect(await live.finish()).toEqual([
		{ push: { index: 0, value: { n: 0 } } },
		{ push: { index: 1, value: { n: 1 } } },
		{ pop: 0 },
		{ push: { index: 2, value: { n: 2 } } },
	]);
});

test("rolling is invisible to the consumer", async () => {
	// The same edits, framed two ways: one group for everything, versus a roll per edit.
	const edits = async (opRatio: number) => {
		const live = new Live({ opRatio });
		for (let n = 0; n < 6; n++) {
			await live.push(n);
			if (n >= 3) await live.pop(1);
		}
		return live.finish();
	};

	expect(await edits(1000)).toEqual(await edits(0));
});

test("compressed round-trip across rolls", async () => {
	const live = new Live({ compression: true, opRatio: 1 });
	for (let n = 0; n < 40; n++) {
		await live.push(n);
		if (n >= 10) await live.pop(1);
	}

	// A tight ratio rolls many times; every record still arrives exactly once, in order.
	expect(pushed(await live.finish())).toEqual(Array.from({ length: 40 }, (_, n) => n));
});

test("the window slides", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Rec>(track);
	for (let n = 0; n < 5; n++) {
		producer.push({ n });
		if (n >= 2) producer.pop(1);
	}

	expect(producer.offset).toBe(3);
	expect(producer.window).toEqual([{ n: 3 }, { n: 4 }]);
});

test("a pop is clamped to the window", async () => {
	const live = new Live();
	await live.push(0);
	await live.pop(9);
	await live.push(1);

	expect(await live.finish()).toEqual([
		{ push: { index: 0, value: { n: 0 } } },
		{ pop: 0 },
		{ push: { index: 1, value: { n: 1 } } },
	]);
});

test("an empty pop writes nothing", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Rec>(track);
	producer.pop(5);
	producer.finish();

	// Nothing was ever pushed, so there is nothing to drop and no group to publish.
	expect(track.subscribe().latest()).toBeUndefined();
});

test("a lagging consumer is told what it missed", async () => {
	// Ops disabled, so every edit rolls: a reader that stops polling really does lose groups.
	const track = new Track.Producer("test");
	const producer = new Producer<Rec>(track, { opRatio: 0 });
	const live = new Live();
	live.producer = producer;
	live.consumer = new Consumer<Rec>(track.subscribe());

	await live.push(0);
	await live.push(1);
	expect(live.events).toEqual([{ push: { index: 0, value: { n: 0 } } }, { push: { index: 1, value: { n: 1 } } }]);

	// The consumer stops reading while the window slides past everything it holds.
	for (let n = 2; n < 8; n++) {
		producer.push({ n });
		producer.pop(1);
	}
	const events = await live.finish();

	const skipped = events.flatMap((e) => ("skip" in e ? [e.skip] : []));
	expect(skipped.length).toBeGreaterThan(0);
	expect(skipped[0]).toBe(2);

	// Every index is still accounted for exactly once, in order.
	const reported = events.flatMap((e) => ("push" in e ? [e.push.index] : "skip" in e ? [e.skip] : []));
	for (let i = 1; i < reported.length; i++) {
		expect(reported[i]).toBe((reported[i - 1] as number) + 1);
	}
});
