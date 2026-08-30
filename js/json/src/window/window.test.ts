import { expect, test } from "bun:test";
import { Track } from "@moq/net";
import { Consumer } from "./consumer.ts";
import { Decoder, Encoder, type Event, Producer, type Span } from "./index.ts";

type Rec = { n: number };

/**
 * A producer and a consumer that reads after every edit.
 *
 * Polling as the publisher goes is what "keeping up" means: a consumer left until the end is a whole
 * group behind, and the default subscription abandons a group as soon as a newer one exists, so it
 * would resume at the newest header instead of reading the rolls in between.
 */
class Live {
	producer: Producer<Rec>;
	consumer: Consumer<Rec>;
	events: Event<Rec>[] = [];
	#next?: Promise<Event<Rec> | undefined>;

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

	// Drain whatever is decodable right now. Keep the pending read when the timer wins, since an
	// abandoned `next()` would still consume a later event with nobody left to observe its result.
	async read(): Promise<void> {
		for (;;) {
			const idle = Symbol("idle");
			this.#next ??= this.consumer.next();
			const event = await Promise.race([
				this.#next,
				new Promise<typeof idle>((resolve) => setTimeout(() => resolve(idle), 0)),
			]);
			if (event === idle || event === undefined) return;
			this.#next = undefined;
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
const span = ({ start, end }: Span): number[] => Array.from({ length: end - start }, (_, i) => start + i);

test("push and pop round-trip", async () => {
	const live = new Live();
	await live.push(0);
	await live.push(1);
	await live.pop(1);
	await live.push(2);

	expect(await live.finish()).toEqual([
		{ push: { index: 0, value: { n: 0 } } },
		{ push: { index: 1, value: { n: 1 } } },
		{ pop: { start: 0, end: 1 } },
		{ push: { index: 2, value: { n: 2 } } },
	]);
});

test("a popped record is never restated", async () => {
	// Ops disabled, so every single edit is its own group restating the whole window.
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
		{ pop: { start: 0, end: 1 } },
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
		{ pop: { start: 0, end: 1 } },
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
	const subscriber = track.subscribe();
	live.consumer = new Consumer<Rec>(subscriber);

	await live.push(0);
	await live.push(1);
	expect(live.events).toEqual([{ push: { index: 0, value: { n: 0 } } }, { push: { index: 1, value: { n: 1 } } }]);

	// The consumer stops reading while the window slides past everything it holds.
	for (let n = 2; n < 8; n++) {
		producer.push({ n });
		producer.pop(1);
	}
	subscriber.startAt(subscriber.latest() as number);
	const events = await live.finish();

	const skipped = events.flatMap((e) => ("skip" in e ? span(e.skip) : []));
	expect(skipped.length).toBeGreaterThan(0);
	expect(skipped[0]).toBe(2);

	// Every index is still accounted for exactly once, in order.
	const reported = events.flatMap((e) => ("push" in e ? [e.push.index] : "skip" in e ? span(e.skip) : []));
	for (let i = 1; i < reported.length; i++) {
		expect(reported[i]).toBe((reported[i - 1] as number) + 1);
	}
});

test("a fresh consumer adopts the current offset", async () => {
	const track = new Track.Producer("test");
	const producer = new Producer<Rec>(track, { opRatio: 0 });
	for (let n = 0; n < 5; n++) producer.push({ n });
	producer.pop(3);

	const subscriber = track.subscribe();
	subscriber.startAt(subscriber.latest() as number);
	const consumer = new Consumer<Rec>(subscriber);
	producer.finish();
	const events: Event<Rec>[] = [];
	for await (const event of consumer) events.push(event);

	expect(events).toEqual([{ push: { index: 3, value: { n: 3 } } }, { push: { index: 4, value: { n: 4 } } }]);
});

test("a large gap is one skip event", () => {
	const decoder = new Decoder<unknown>();
	decoder.decode(new TextEncoder().encode('{"offset":0,"records":[]}'));
	decoder.reset();
	decoder.decode(new TextEncoder().encode(`{"offset":${Number.MAX_SAFE_INTEGER},"records":[]}`));

	expect(decoder.next()).toEqual({ skip: { start: 0, end: Number.MAX_SAFE_INTEGER } });
	expect(decoder.next()).toBeUndefined();
});

test("every group requires a header", () => {
	const decoder = new Decoder<unknown>();
	decoder.decode(new TextEncoder().encode('{"offset":0,"records":[]}'));
	decoder.reset();

	expect(() => decoder.decode(new TextEncoder().encode('{"push":null}'))).toThrow("window header records");
});

test("a header is only valid as frame zero", () => {
	const decoder = new Decoder<unknown>();
	decoder.decode(new TextEncoder().encode('{"offset":0,"records":[]}'));

	expect(() => decoder.decode(new TextEncoder().encode('{"offset":0,"records":[]}'))).toThrow(
		"exactly one operation",
	);
});

test("pop counts are nonnegative safe integers", () => {
	const encoder = new Encoder<unknown>();
	for (const count of [-1, 0.5, Number.NaN, Number.POSITIVE_INFINITY]) {
		expect(() => encoder.pop(count)).toThrow("pop count must be a nonnegative safe integer");
	}
	expect(encoder.offset).toBe(0);
	expect(encoder.window).toEqual([]);
});

test("an op that would evict the header rolls first", () => {
	const encoder = new Encoder<string>({ opRatio: 0xffffffff });
	const first = "a".repeat(16 * 1024 * 1024);
	const next = "b".repeat(15 * 1024 * 1024);

	let frame = encoder.push(first);
	expect(frame.keyframe).toBeTrue();
	frame.commit();

	frame = encoder.push(next);
	expect(frame.keyframe).toBeFalse();
	frame.commit();

	const popped = encoder.pop(1);
	if (!popped) throw new Error("expected a pop frame");
	frame = popped;
	expect(frame.keyframe).toBeFalse();
	frame.commit();

	frame = encoder.push(next);
	expect(frame.keyframe).toBeTrue();
	frame.commit();
});
