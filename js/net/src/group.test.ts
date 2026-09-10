import { expect, test } from "bun:test";
import { FrameTooLarge, GroupTooLarge, MAX_GROUP_CACHE_BYTES, MAX_GROUP_FRAMES, Producer } from "./group.ts";
import { Timestamp } from "./time.ts";

const dec = new TextDecoder();

test("used reflects mirror demand and unused resolves when the last reader leaves", async () => {
	const producer = new Producer(0);

	// No mirror readers: no demand.
	expect(producer.used.peek()).toBe(false);

	const a = producer.mirror();
	const b = producer.mirror();
	expect(producer.used.peek()).toBe(true);

	// Closing one of two keeps demand, so unused() stays pending.
	a.close();
	expect(producer.used.peek()).toBe(true);

	// Closing the last reader drops demand; unused() resolves. Fetch coalescing awaits this to
	// cancel a download that everyone has abandoned (a group may never end on its own).
	b.close();
	await producer.unused();
	expect(producer.used.peek()).toBe(false);
});

function pair(sequence: number) {
	const producer = new Producer(sequence);
	return { producer, consumer: producer.consume() };
}

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
	producer.close();

	expect(() => producer.writeFrame({ payload: new Uint8Array([0]), timestamp: Timestamp.now() })).toThrow(
		"group is closed",
	);
	expect(producer.closed.peek()).toBeNull();
	expect((await consumer.readFrame())?.payload[0]).toBe(0);
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
	producer.close();

	expect((await consumer.readFrame())?.payload[0]).toBe(1);
	expect((await consumer.readFrame())?.payload[0]).toBe(2);
	expect(await consumer.readFrame()).toBeUndefined();
});

test("tryReadFrame drains buffered frames then returns undefined", () => {
	const { producer, consumer } = pair(0);
	producer.writeString("a");
	producer.writeString("b");

	expect(dec.decode(consumer.tryReadFrame()?.payload)).toBe("a");
	expect(dec.decode(consumer.tryReadFrame()?.payload)).toBe("b");
	// Nothing buffered: undefined, and the group is not closed so this is not end-of-group.
	expect(consumer.tryReadFrame()).toBeUndefined();
});

test("tryReadFrameSequence reports per-frame sequence numbers", () => {
	const { producer, consumer } = pair(7);
	producer.writeString("a");
	producer.writeString("b");

	expect(consumer.tryReadFrameSequence()).toMatchObject({ sequence: 0, payload: new TextEncoder().encode("a") });
	expect(consumer.tryReadFrameSequence()).toMatchObject({ sequence: 1, payload: new TextEncoder().encode("b") });
	expect(consumer.tryReadFrameSequence()).toBeUndefined();
});

test("readFrameSequence reports per-frame sequence numbers", async () => {
	const { producer, consumer } = pair(7);
	producer.writeString("a");
	producer.writeString("b");

	expect(await consumer.readFrameSequence()).toMatchObject({ sequence: 0, payload: new TextEncoder().encode("a") });
	expect(await consumer.readFrameSequence()).toMatchObject({ sequence: 1, payload: new TextEncoder().encode("b") });
});

test("done distinguishes a finished group from one that is merely empty", () => {
	const { producer, consumer } = pair(0);
	// Open and empty: not done (more frames may arrive), and tryReadFrame is undefined.
	expect(consumer.tryReadFrame()).toBeUndefined();
	expect(consumer.done).toBe(false);

	producer.writeString("a");
	// Buffered but closed: still not done until the frame is drained.
	producer.close();
	expect(consumer.done).toBe(false);

	consumer.tryReadFrame();
	// Drained and closed: now done.
	expect(consumer.tryReadFrame()).toBeUndefined();
	expect(consumer.done).toBe(true);
});

test("a previously obtained closed handle peeks source closure synchronously", () => {
	const { producer, consumer } = pair(0);
	const closed = consumer.closed;

	producer.close();

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
	producer.writeString("hi");
	await readable; // must not hang
	expect(dec.decode(consumer.tryReadFrame()?.payload)).toBe("hi");
});

test("readable resolves once the group closes, even with nothing buffered", async () => {
	const { producer, consumer } = pair(0);
	const readable = consumer.readable();
	producer.close();
	await readable; // resolves on close so callers don't wait forever
	expect(consumer.tryReadFrame()).toBeUndefined();
});

test("buffered frames are still readable after the group closes", async () => {
	const { producer, consumer } = pair(0);
	producer.writeString("a");
	producer.close();

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
	producer.close();
	expect(consumer.tryReadFrame()).toBeUndefined();
});
