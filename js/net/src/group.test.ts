import { expect, test } from "bun:test";
import { Manual, Milli, Nano, yieldNow } from "@moq/clock";
import { Signal } from "@moq/signals";
import { FrameTooLarge, GroupTooLarge, StreamCode, StreamError } from "./error.ts";
import { MAX_GROUP_CACHE_BYTES, MAX_GROUP_FRAMES, Producer } from "./group.ts";
import { hooks, type Replacement } from "./internal.ts";
import { textFrame } from "./mock.ts";
import { Timestamp } from "./time.ts";
import { Producer as TrackProducer } from "./track.ts";

const dec = new TextDecoder();

test("a transport offset costs no frame storage and proves no group start", async () => {
	const offset = MAX_GROUP_FRAMES + 7;
	const producer = new Producer(0);
	hooks.startGroup(producer, offset);
	const early = producer.mirror();
	expect(hooks.groupStarted(early)).toBe(false);
	expect(hooks.groupTimestamp(early)).toBeUndefined();
	for (let i = 0; i < MAX_GROUP_FRAMES; i++)
		producer.writeFrame({ payload: new Uint8Array([i % 256]), timestamp: Timestamp.fromMillis(i) });
	const late = producer.mirror();
	for (const reader of [early, late]) {
		expect(hooks.groupStarted(reader)).toBe(true);
		expect(hooks.groupTimestamp(reader)).toBeUndefined();
		for (let i = 0; i < MAX_GROUP_FRAMES; i++) {
			const frame = await reader.readFrameSequence();
			expect(frame?.sequence).toBe(offset + i);
			expect(frame?.payload[0]).toBe(i % 256);
		}
		reader.close();
	}
	expect(() => producer.writeFrame(textFrame("overflow"))).toThrow(GroupTooLarge);
});

test("a compressed group cannot start without its dictionary prefix", () => {
	const producer = new Producer(0);
	hooks.startGroup(producer, 5);
	expect(() => hooks.bindGroupCompression(producer, true)).toThrow("compressed group lacks its prefix");
	producer.finish();
});

test("reordered frames keep the highest group PTS in live and later mirrors", () => {
	const installed = new Manual().install();
	try {
		const producer = new Producer(0);
		const shared = producer.consume();
		const live = producer.mirror();
		for (const timestamp of [Timestamp.fromMillis(0), Timestamp.fromMicros(80_000), Timestamp.fromMillis(40)])
			producer.writeFrame({ payload: new Uint8Array([1]), timestamp });
		expect(hooks.groupTimestamp(shared)?.asMillis()).toBe(0);
		expect(hooks.groupLatest(shared)?.asMillis()).toBe(80);
		expect(hooks.groupLatest(live)?.asMillis()).toBe(80);
		// A reader left with only the B-frame keeps the high PTS it already read past.
		shared.tryReadFrame();
		shared.tryReadFrame();
		expect(hooks.groupLatest(shared)?.asMillis()).toBe(80);
		// Reads are per handle, so a later mirror still replays every frame.
		const later = producer.mirror();
		expect(hooks.groupLatest(later)?.asMillis()).toBe(80);
		expect(later.tryReadFrame()?.timestamp?.asMillis()).toBe(0);
		for (const reader of [shared, live, later]) reader.close();
		producer.finish();
	} finally {
		installed.close();
	}
});

test("a frame read late keeps the instant it landed, plain, compressed, or resumed", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const plain = new Producer(0);
		const compressed = new Producer(0);
		// Frame 0 lands before compression binds, so the restored copy must carry its stamp too.
		compressed.writeFrame(textFrame("a"));
		hooks.bindGroupCompression(compressed, true);
		const first = clock.now();
		plain.writeFrame(textFrame("a"));
		clock.jump(Nano.fromMilli(Milli(10)));
		const second = clock.now();
		plain.writeFrame(textFrame("b"));
		compressed.writeFrame(textFrame("b"));
		clock.jump(Nano.fromMilli(Milli(1000)));

		const reader = plain.consume();
		expect(reader.tryReadFrame()?.arrived).toBe(first);
		expect((await reader.readFrameSequence())?.arrived).toBe(second);
		const stored = compressed.consume();
		expect((await hooks.readGroupFrame(stored, true))?.frame.arrived).toBe(first);
		expect((await stored.readFrame())?.arrived).toBe(second);

		// A frame written in with a stamp of its own still gets this node's.
		const forwarded = new Producer(0);
		forwarded.writeFrame({ payload: new Uint8Array([1]), arrived: first });
		expect(forwarded.consume().tryReadFrame()?.arrived).toBe(clock.now());

		// A resumed read takes the stamp the replacement copy holds, not when it was adopted.
		const failing = new Producer(0);
		const resumed = failing.mirror();
		const replacement = new Producer(0);
		replacement.writeFrame(textFrame("a"));
		const landed = clock.now();
		replacement.writeFrame(textFrame("b"));
		hooks.recoverGroup(resumed, {
			changed: new Signal(0),
			wants: (failed) => failed !== undefined,
			replace: async () => ({
				group: replacement.mirror(),
				expiry: { expired: () => false, changed: [] },
				adopt: () => {},
				discard: () => {},
			}),
		});
		failing.writeFrame(textFrame("a"));
		await resumed.readFrame();
		clock.jump(Nano.fromMilli(Milli(1000)));
		failing.abort(new Error("copy failed"));
		expect((await resumed.readFrame())?.arrived).toBe(landed);

		for (const producer of [plain, compressed, forwarded, replacement]) producer.finish();
	} finally {
		installed.close();
	}
});

test("a group read late keeps the instant it joined, alone, on a track, or resumed", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const created = clock.now();
		const alone = new Producer(0);
		const track = new TrackProducer("test");
		const subscriber = track.subscribe();
		const live = track.createGroup(0);
		live.writeFrame(textFrame("a"));
		clock.jump(Nano.fromMilli(Milli(1000)));

		expect(alone.consume().arrived).toBe(created);
		expect((await subscriber.recvGroup())?.arrived).toBe(created);

		// A resumed read keeps the original group's arrival, not the replacement copy's.
		const original = clock.now();
		const failing = new Producer(0);
		const resumed = failing.mirror();
		clock.jump(Nano.fromMilli(Milli(1000)));
		const replacement = new Producer(0);
		replacement.writeFrame(textFrame("a"));
		replacement.writeFrame(textFrame("b"));
		hooks.recoverGroup(resumed, {
			changed: new Signal(0),
			wants: (failed) => failed !== undefined,
			replace: async () => ({
				group: replacement.mirror(),
				expiry: { expired: () => false, changed: [] },
				adopt: () => {},
				discard: () => {},
			}),
		});
		failing.writeFrame(textFrame("a"));
		await resumed.readFrame();
		failing.abort(new Error("copy failed"));
		expect((await resumed.readFrame())?.payload).toEqual(textFrame("b").payload);
		expect(resumed.arrived).toBe(original);

		for (const producer of [alone, live, replacement]) producer.finish();
		subscriber.close();
		track.close();
	} finally {
		installed.close();
	}
});

test("zero group PTS keeps its scale and untimed groups stay unstamped", () => {
	const installed = new Manual().install();
	try {
		for (const timestamp of [undefined, Timestamp.fromMicros(0)]) {
			const producer = new Producer(0);
			const consumer = producer.consume();
			expect(hooks.groupLatest(consumer)).toBeUndefined();
			producer.writeFrame({ payload: new Uint8Array(), timestamp });
			expect(hooks.groupLatest(consumer)).toBe(timestamp);
			consumer.close();
			producer.finish();
		}
	} finally {
		installed.close();
	}
});

test("wanted reflects mirror demand until the last reader leaves", async () => {
	const producer = new Producer(0);

	// No mirror readers: no demand.
	expect(producer.demand().wanted.peek()).toBe(false);

	const a = producer.mirror();
	const b = producer.mirror();
	expect(producer.demand().wanted.peek()).toBe(true);

	// Closing one of two keeps demand.
	a.close();
	expect(producer.demand().wanted.peek()).toBe(true);

	// Closing the last reader drops demand. Fetch coalescing watches this to cancel a download
	// that everyone has abandoned (a group may never end on its own).
	b.close();
	const wanted = producer.demand().wanted;
	while (wanted.peek()) await wanted.changed();
	expect(wanted.peek()).toBe(false);
});

function pair(sequence: number) {
	const producer = new Producer(sequence);
	return { producer, consumer: producer.consume() };
}

test("closing a consumer releases only that handle", async () => {
	const { producer, consumer } = pair(0);
	const other = producer.consume();
	producer.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.now() });

	consumer.close();
	expect(await consumer.closed).toBeNull();
	expect(producer.closed.peek()).toBeUndefined();

	// The group stays writable, and another reader sees every frame and the end.
	producer.writeFrame({ payload: new Uint8Array([2]), timestamp: Timestamp.now() });
	producer.finish();
	expect((await other.readFrame())?.payload[0]).toBe(1);
	expect((await other.readFrame())?.payload[0]).toBe(2);
	expect(await other.readFrame()).toBeUndefined();
});

test("closing a consumer ends its own parked reads", async () => {
	const { producer, consumer } = pair(0);
	const other = producer.consume();
	const reading = consumer.readFrame();
	const waiting = consumer.readable();
	consumer.close();
	expect(await reading).toBeUndefined();
	await waiting;
	expect(consumer.isClosed).toBe(true);
	expect(consumer.done).toBe(true);

	// The other reader is still parked on the open group, and reads the next frame.
	const next = other.readFrame();
	producer.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.now() });
	expect((await next)?.payload[0]).toBe(1);
	expect(other.isClosed).toBe(false);

	// A later write never reaches the released handle, so it takes nothing from the other reader.
	producer.writeFrame({ payload: new Uint8Array([2]), timestamp: Timestamp.now() });
	expect(consumer.done).toBe(true);
	expect(consumer.tryReadFrame()).toBeUndefined();
	expect(await consumer.readFrame()).toBeUndefined();
	expect((await other.readFrame())?.payload[0]).toBe(2);
	producer.finish();
});

test("a released consumer ignores a later abort", async () => {
	const { producer, consumer } = pair(0);
	const reading = consumer.readFrame();
	consumer.close();
	producer.abort(new Error("upstream"));
	expect(await reading).toBeUndefined();
	expect(await consumer.readFrame()).toBeUndefined();
	expect(await consumer.closed).toBeNull();
});

test("a group caps its frame count and aborts on the next write", async () => {
	const { producer, consumer } = pair(0);

	for (let i = 0; i < MAX_GROUP_FRAMES; i++) {
		producer.writeFrame({ payload: new Uint8Array([i & 0xff]), timestamp: Timestamp.now() });
	}
	expect(() => producer.writeFrame({ payload: new Uint8Array([0]), timestamp: Timestamp.now() })).toThrow(
		GroupTooLarge,
	);

	await expect(consumer.readFrame()).rejects.toBeInstanceOf(GroupTooLarge);
});

test("a group caps its byte size and aborts on the next write", async () => {
	const { producer, consumer } = pair(0);

	const oneMiB = 1024 * 1024;
	for (let i = 0; i < MAX_GROUP_CACHE_BYTES / oneMiB; i++) {
		producer.writeFrame({ payload: new Uint8Array(oneMiB), timestamp: Timestamp.now() });
	}
	expect(() => producer.writeFrame({ payload: new Uint8Array(1), timestamp: Timestamp.now() })).toThrow(
		GroupTooLarge,
	);

	await expect(consumer.readFrame()).rejects.toBeInstanceOf(GroupTooLarge);
});

test("a caught-up reader still trips the byte cache cap", async () => {
	const { producer, consumer } = pair(0);

	const oneMiB = 1024 * 1024;
	const frames = MAX_GROUP_CACHE_BYTES / oneMiB;
	for (let i = 0; i < frames; i++) {
		producer.writeFrame({ payload: new Uint8Array(oneMiB), timestamp: Timestamp.now() });
		expect((await consumer.readFrame())?.payload.byteLength).toBe(oneMiB);
	}
	expect(() => producer.writeFrame({ payload: new Uint8Array(oneMiB), timestamp: Timestamp.now() })).toThrow(
		GroupTooLarge,
	);
});

test("the writer sees GroupTooLarge when the group overflows", () => {
	const producer = new Producer(0);
	for (let i = 0; i < MAX_GROUP_FRAMES; i++) {
		producer.writeFrame({ payload: new Uint8Array([i & 0xff]), timestamp: Timestamp.now() });
	}
	expect(() => producer.writeFrame({ payload: new Uint8Array([0]), timestamp: Timestamp.now() })).toThrow(
		GroupTooLarge,
	);
	expect(producer.closed.peek()).toBeInstanceOf(GroupTooLarge);
});

test("a closed group rejects a further write without discarding committed frames", async () => {
	const { producer, consumer } = pair(0);
	for (let i = 0; i < MAX_GROUP_FRAMES; i++) {
		producer.writeFrame({ payload: new Uint8Array([i & 0xff]), timestamp: Timestamp.now() });
	}
	producer.finish();

	expect(() => producer.writeFrame({ payload: new Uint8Array([0]), timestamp: Timestamp.now() })).toThrow(
		"group is closed",
	);
	expect(producer.closed.peek()).toBeNull();
	expect((await consumer.readFrame())?.payload[0]).toBe(0);
});

test("an abort after finish keeps the clean outcome", async () => {
	const { producer, consumer } = pair(0);
	producer.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.now() });
	producer.finish();
	producer.abort(new Error("boom"));

	expect(producer.closed.peek()).toBeNull();
	expect((await consumer.readFrame())?.payload[0]).toBe(1);
	expect(await consumer.readFrame()).toBeUndefined();
});

test("a read that starts later skips frames below it instead of returning them", async () => {
	const { producer, consumer } = pair(0);

	for (let i = 0; i < 10; i++) {
		producer.writeFrame({ payload: new Uint8Array([i]), timestamp: Timestamp.now() });
	}

	expect(await consumer.readFrameSequence({ from: 5 })).toMatchObject({
		sequence: 5,
		payload: new Uint8Array([5]),
	});
	expect(await consumer.readFrameSequence({ from: 5 })).toMatchObject({ sequence: 6 });
});

test("a group with no eviction reads every frame without error", async () => {
	const { producer, consumer } = pair(0);
	producer.writeFrame({ payload: new Uint8Array([1]), timestamp: Timestamp.now() });
	producer.writeFrame({ payload: new Uint8Array([2]), timestamp: Timestamp.now() });
	producer.finish();

	expect((await consumer.readFrame())?.payload[0]).toBe(1);
	expect((await consumer.readFrame())?.payload[0]).toBe(2);
	expect(await consumer.readFrame()).toBeUndefined();
});

test("tryReadFrame drains buffered frames then returns undefined", () => {
	const { producer, consumer } = pair(0);
	producer.writeFrame(textFrame("a"));
	producer.writeFrame(textFrame("b"));

	expect(dec.decode(consumer.tryReadFrame()?.payload)).toBe("a");
	expect(dec.decode(consumer.tryReadFrame()?.payload)).toBe("b");
	// Nothing buffered: undefined, and the group is not closed so this is not end-of-group.
	expect(consumer.tryReadFrame()).toBeUndefined();
});

test("tryReadFrameSequence reports per-frame sequence numbers", () => {
	const { producer, consumer } = pair(7);
	producer.writeFrame(textFrame("a"));
	producer.writeFrame(textFrame("b"));

	expect(consumer.tryReadFrameSequence()).toMatchObject({ sequence: 0, payload: new TextEncoder().encode("a") });
	expect(consumer.tryReadFrameSequence()).toMatchObject({ sequence: 1, payload: new TextEncoder().encode("b") });
	expect(consumer.tryReadFrameSequence()).toBeUndefined();
});

test("readFrameSequence reports per-frame sequence numbers", async () => {
	const { producer, consumer } = pair(7);
	producer.writeFrame(textFrame("a"));
	producer.writeFrame(textFrame("b"));

	expect(await consumer.readFrameSequence()).toMatchObject({ sequence: 0, payload: new TextEncoder().encode("a") });
	expect(await consumer.readFrameSequence()).toMatchObject({ sequence: 1, payload: new TextEncoder().encode("b") });
});

test("done distinguishes a finished group from one that is merely empty", () => {
	const { producer, consumer } = pair(0);
	// Open and empty: not done (more frames may arrive), and tryReadFrame is undefined.
	expect(consumer.tryReadFrame()).toBeUndefined();
	expect(consumer.done).toBe(false);

	producer.writeFrame(textFrame("a"));
	// Buffered but closed: still not done until the frame is drained.
	producer.finish();
	expect(consumer.done).toBe(false);

	consumer.tryReadFrame();
	// Drained and closed: now done.
	expect(consumer.tryReadFrame()).toBeUndefined();
	expect(consumer.done).toBe(true);
});

test("a previously obtained closed handle peeks source closure synchronously", () => {
	const { producer, consumer } = pair(0);
	const closed = consumer.closed;

	producer.finish();

	expect(closed.peek()).toBeNull();
});

test("readable resolves once a frame is buffered", async () => {
	const { producer, consumer } = pair(0);
	// No frame yet: readable() must stay pending for an empty, open group.
	const readable = consumer.readable();
	let settled = false;
	void readable.then(() => {
		settled = true;
	});
	await Promise.resolve();
	expect(settled).toBe(false);

	// Writing makes it resolve.
	producer.writeFrame(textFrame("hi"));
	await readable; // must not hang
	expect(dec.decode(consumer.tryReadFrame()?.payload)).toBe("hi");
});

test("readable resolves once the group closes, even with nothing buffered", async () => {
	const { producer, consumer } = pair(0);
	const readable = consumer.readable();
	producer.finish();
	await readable; // resolves on close so callers don't wait forever
	expect(consumer.tryReadFrame()).toBeUndefined();
});

test("readable waits past replayed frames after adopting an open replacement", async () => {
	const producer = new Producer(0);
	const consumer = producer.mirror();
	const replacement = new Producer(0);
	const route = new Signal(false);
	const adopted = Promise.withResolvers<void>();
	hooks.recoverGroup(consumer, {
		changed: route,
		wants: () => route.peek(),
		replace: async (index) => {
			expect(index).toBe(1);
			return {
				group: replacement.mirror(),
				expiry: { expired: () => false, changed: [] },
				adopt: () => {
					route.set(false);
					adopted.resolve();
				},
				discard: () => {},
			};
		},
	});
	producer.writeFrame(textFrame("zero"));
	expect(await consumer.readString()).toBe("zero");
	replacement.writeFrame(textFrame("zero"));
	route.set(true);

	let settled = false;
	const readable = consumer.readable().then(() => {
		settled = true;
	});
	await adopted.promise;
	// Drain the adoption's microtasks without advancing wall-clock time.
	await new Promise<void>((resolve) => setImmediate(resolve));
	expect(settled).toBe(false);

	replacement.writeFrame(textFrame("one"));
	await readable;
	expect(consumer.tryReadFrameSequence()).toMatchObject({ sequence: 1, payload: textFrame("one").payload });
	consumer.close();
	producer.finish();
	replacement.finish();
});

test("done ignores replayed frames an adopted copy buffers", async () => {
	const producer = new Producer(0);
	const consumer = producer.mirror();
	const replacement = new Producer(0);
	const route = new Signal(false);
	const adopted = Promise.withResolvers<void>();
	hooks.recoverGroup(consumer, {
		changed: route,
		wants: () => route.peek(),
		replace: async () => ({
			group: replacement.mirror(),
			expiry: { expired: () => false, changed: [] },
			adopt: () => {
				route.set(false);
				adopted.resolve();
			},
			discard: () => {},
		}),
	});
	producer.writeFrame(textFrame("zero"));
	expect(await consumer.readString()).toBe("zero");
	route.set(true);

	const readable = consumer.readable();
	await adopted.promise;
	// Drain the adoption's microtasks without advancing wall-clock time.
	await new Promise<void>((resolve) => setImmediate(resolve));
	// The adopted copy ends having repeated only the frame already read.
	replacement.writeFrame(textFrame("zero"));
	replacement.finish();
	expect(consumer.done).toBe(true);
	await readable;
	expect(consumer.tryReadFrame()).toBeUndefined();
	producer.finish();
});

test("buffered frames are still readable after the group closes", async () => {
	const { producer, consumer } = pair(0);
	producer.writeFrame(textFrame("a"));
	producer.finish();

	// Closing doesn't discard buffered frames; the blocking reader drains them before ending.
	expect(await consumer.readString()).toBe("a");
	expect(await consumer.readFrame()).toBeUndefined();
});

test("a frame larger than the cache is rejected rather than silently dropped", () => {
	// A silent success would report a write that nothing can ever read. Rust rejects the same
	// frame up front with `Error::FrameTooLarge`.
	const producer = new Producer(0);
	const oversized = new Uint8Array(MAX_GROUP_CACHE_BYTES + 1);

	expect(() => producer.writeFrame({ payload: oversized, timestamp: Timestamp.now() })).toThrow(FrameTooLarge);

	// Nothing was appended, so the group is still empty rather than holding a phantom frame.
	const consumer = producer.consume();
	producer.finish();
	expect(consumer.tryReadFrame()).toBeUndefined();
});

test("a group that ends on its own abandons the replacement it asked for", async () => {
	const producer = new Producer(0);
	const consumer = producer.mirror();
	const route = new Signal(0);
	let signal: AbortSignal | undefined;
	hooks.recoverGroup(consumer, {
		changed: route,
		// A stall looks past the copy once the route moved.
		wants: (failed) => failed !== undefined || route.peek() > 0,
		replace: (_index, _failed, abort) => {
			signal = abort;
			return new Promise(() => {});
		},
	});
	producer.writeFrame({ payload: new Uint8Array([0]) });
	await consumer.readFrame();

	route.set(1);
	const read = consumer.readFrame();
	await yieldNow();
	expect(signal?.aborted).toBe(false);

	producer.finish();
	expect(await read).toBeUndefined();
	expect(signal?.aborted).toBe(true);
});

test.each([false, true])("a group ending beside its replacement discards it (consumer close: %p)", async (close) => {
	const producer = new Producer(0);
	const consumer = producer.mirror();
	const route = new Signal(0);
	const replacement = new Producer(0);
	let resolve: ((value: Replacement) => void) | undefined;
	let adopted = false;
	let discarded = false;
	let released = 0;
	const requested = Promise.withResolvers<void>();
	hooks.recoverGroup(consumer, {
		changed: route,
		close: () => {
			released++;
		},
		wants: (failed) => failed !== undefined || route.peek() > 0,
		replace: () =>
			new Promise((r) => {
				resolve = r;
				requested.resolve();
			}),
	});
	producer.writeFrame({ payload: new Uint8Array([0]) });
	await consumer.readFrame();

	route.set(1);
	const read = consumer.readFrame();
	await requested.promise;
	// Both land before the read looks again.
	resolve?.({
		group: replacement.mirror(),
		expiry: { expired: () => false, changed: [] },
		adopt: () => {
			adopted = true;
		},
		discard: () => {
			discarded = true;
		},
	});
	if (close) {
		consumer.close();
		consumer.close();
		expect(released).toBe(1);
	} else {
		producer.finish();
	}
	expect(await read).toBeUndefined();
	expect(adopted).toBe(false);
	expect(discarded).toBe(true);
});

test("a group recovered twice reads on and settles", async () => {
	const producer = new Producer(0);
	const consumer = producer.mirror();
	const route = new Signal(0);
	const replacements = [new Producer(0), new Producer(0)];
	let next = 0;
	hooks.recoverGroup(consumer, {
		changed: route,
		wants: (failed) => failed !== undefined,
		replace: async () => ({
			group: replacements[next++].mirror(),
			expiry: { expired: () => false, changed: [] },
			adopt: () => {},
			discard: () => {},
		}),
	});
	void consumer.closed;
	producer.writeFrame({ payload: new Uint8Array([0]) });
	await consumer.readFrame();

	// Two failures in a row: each moves the group to the next copy.
	producer.abort(new Error("route failed"));
	replacements[0].writeFrame({ payload: new Uint8Array([0]) });
	replacements[0].writeFrame({ payload: new Uint8Array([1]) });
	expect((await consumer.readFrame())?.payload[0]).toBe(1);
	replacements[0].abort(new Error("route failed"));
	for (const payload of [0, 1, 2]) replacements[1].writeFrame({ payload: new Uint8Array([payload]) });
	replacements[1].finish();
	expect((await consumer.readFrame())?.payload[0]).toBe(2);
	expect(await consumer.readFrame()).toBeUndefined();
	expect(await consumer.closed).toBeNull();
});

test("a stalled group whose copy then fails asks again with the failure", async () => {
	const producer = new Producer(0);
	const consumer = producer.mirror();
	const route = new Signal(0);
	const verdict = new Error("refused");
	hooks.recoverGroup(consumer, {
		changed: route,
		wants: (failed) => failed !== undefined || route.peek() > 0,
		// A stall waits for the route; a failure gets the route's verdict.
		replace: (_index, failed) => (failed ? Promise.reject(verdict) : new Promise(() => {})),
	});
	producer.writeFrame({ payload: new Uint8Array([0]) });
	await consumer.readFrame();

	route.set(1);
	const read = consumer.readFrame();
	await yieldNow();
	producer.abort(new Error("route failed"));
	await expect(read).rejects.toBe(verdict);
});

test("a drained unfinished group fails with Old when its budget gives up", async () => {
	const { producer, consumer } = pair(0);
	producer.writeFrame(textFrame("a"));
	const expired = new Signal(false);
	hooks.expireGroup(consumer, { expired: () => expired.peek(), changed: [expired] });

	expect(await consumer.readString()).toBe("a");
	const pending = consumer.readFrame();
	expired.set(true);

	// The producer never finished the group, so a clean end would pass the prefix off as whole.
	const err = await pending.then(
		() => undefined,
		(e: unknown) => e,
	);
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.Old);
	await expect(consumer.readFrame()).rejects.toBe(err);
	expect(consumer.done).toBe(true);
	expect(await consumer.closed).toBe(err as StreamError);
});
