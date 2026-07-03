import { expect, test } from "bun:test";
import { CacheFull, Group, MAX_GROUP_CACHE_BYTES, MAX_GROUP_FRAMES } from "./group.ts";
import { Timestamp } from "./time.ts";

const dec = new TextDecoder();

test("a group caps its frame count, dropping from the front", () => {
	const group = new Group(0);

	const extra = 100;
	for (let i = 0; i < MAX_GROUP_FRAMES + extra; i++) {
		group.writeFrame({ data: new Uint8Array([i & 0xff]), timestamp: Timestamp.now() });
	}

	// Drain the buffered frames: exactly MAX_GROUP_FRAMES remain after eviction.
	const frames: { sequence: number; data: Uint8Array }[] = [];
	for (let f = group.tryReadFrameSequence(); f; f = group.tryReadFrameSequence()) frames.push(f);
	expect(frames.length).toBe(MAX_GROUP_FRAMES);
	// Sequence numbers count every frame ever written (evicted included), so the first surviving
	// frame is index `extra`, not 0: indices stay consistent across eviction.
	expect(frames[0].sequence).toBe(extra);
	expect(frames[0].data[0]).toBe(extra & 0xff);
});

test("a group caps its byte size, dropping from the front", () => {
	const group = new Group(0);

	// 40 x 1 MiB = 40 MiB, over the 32 MiB cap.
	const oneMiB = 1024 * 1024;
	for (let i = 0; i < 40; i++) {
		group.writeFrame({ data: new Uint8Array(oneMiB), timestamp: Timestamp.now() });
	}

	// Drain the buffered frames and sum their bytes: the cache stayed under the byte cap.
	const frames: Uint8Array[] = [];
	for (let f = group.tryReadFrame(); f; f = group.tryReadFrame()) frames.push(f);
	const bytes = frames.reduce((sum, f) => sum + f.byteLength, 0);
	expect(bytes).toBeLessThanOrEqual(MAX_GROUP_CACHE_BYTES);
	expect(frames.length).toBe(MAX_GROUP_CACHE_BYTES / oneMiB);
});

test("a caught-up reader does not trip the byte cache cap", async () => {
	const group = new Group(0);

	const oneMiB = 1024 * 1024;
	const frames = MAX_GROUP_CACHE_BYTES / oneMiB + 8;
	for (let i = 0; i < frames; i++) {
		group.writeFrame({ data: new Uint8Array(oneMiB), timestamp: Timestamp.now() });
		expect((await group.readFrame())?.data.byteLength).toBe(oneMiB);
	}
});

test("reading a group whose frames were evicted throws CacheFull", async () => {
	const group = new Group(0);

	// Overflow the frame cap without reading, so the front frames are evicted.
	for (let i = 0; i < MAX_GROUP_FRAMES + 10; i++) {
		group.writeFrame({ data: new Uint8Array([i & 0xff]), timestamp: Timestamp.now() });
	}

	// The reader fell behind the eviction window: it must error, not skip the gap.
	expect(group.readFrame()).rejects.toBeInstanceOf(CacheFull);
});

test("a group with no eviction reads every frame without error", async () => {
	const group = new Group(0);
	group.writeFrame({ data: new Uint8Array([1]), timestamp: Timestamp.now() });
	group.writeFrame({ data: new Uint8Array([2]), timestamp: Timestamp.now() });
	group.close();

	expect((await group.readFrame())?.data[0]).toBe(1);
	expect((await group.readFrame())?.data[0]).toBe(2);
	expect(await group.readFrame()).toBeUndefined();
});

test("tryReadFrame drains buffered frames then returns undefined", () => {
	const group = new Group(0);
	group.writeString("a");
	group.writeString("b");

	expect(dec.decode(group.tryReadFrame())).toBe("a");
	expect(dec.decode(group.tryReadFrame())).toBe("b");
	// Nothing buffered: undefined, and the group is not closed so this is not end-of-group.
	expect(group.tryReadFrame()).toBeUndefined();
});

test("tryReadFrameSequence reports per-frame sequence numbers", () => {
	const group = new Group(7);
	group.writeString("a");
	group.writeString("b");

	expect(group.tryReadFrameSequence()).toEqual({ sequence: 0, data: new TextEncoder().encode("a") });
	expect(group.tryReadFrameSequence()).toEqual({ sequence: 1, data: new TextEncoder().encode("b") });
	expect(group.tryReadFrameSequence()).toBeUndefined();
});

test("readFrameSequence reports per-frame sequence numbers", async () => {
	const group = new Group(7);
	group.writeString("a");
	group.writeString("b");

	expect(await group.readFrameSequence()).toEqual({ sequence: 0, data: new TextEncoder().encode("a") });
	expect(await group.readFrameSequence()).toEqual({ sequence: 1, data: new TextEncoder().encode("b") });
});

test("done distinguishes a finished group from one that is merely empty", () => {
	const group = new Group(0);
	// Open and empty: not done (more frames may arrive), and tryReadFrame is undefined.
	expect(group.tryReadFrame()).toBeUndefined();
	expect(group.done).toBe(false);

	group.writeString("a");
	// Buffered but closed: still not done until the frame is drained.
	group.close();
	expect(group.done).toBe(false);

	group.tryReadFrame();
	// Drained and closed: now done.
	expect(group.tryReadFrame()).toBeUndefined();
	expect(group.done).toBe(true);
});

test("readable resolves once a frame is buffered", async () => {
	const group = new Group(0);
	// No frame yet: readable() must stay pending for an empty, open group.
	const readable = group.readable();
	let settled = false;
	void readable.then(() => {
		settled = true;
	});
	await Promise.resolve();
	expect(settled).toBe(false);

	// Writing makes it resolve.
	group.writeString("hi");
	await readable; // must not hang
	expect(dec.decode(group.tryReadFrame())).toBe("hi");
});

test("readable resolves once the group closes, even with nothing buffered", async () => {
	const group = new Group(0);
	const readable = group.readable();
	group.close();
	await readable; // resolves on close so callers don't wait forever
	expect(group.tryReadFrame()).toBeUndefined();
});

test("buffered frames are still readable after the group closes", async () => {
	const group = new Group(0);
	group.writeString("a");
	group.close();

	// Closing doesn't discard buffered frames; the blocking reader drains them before ending.
	expect(await group.readString()).toBe("a");
	expect(await group.readFrame()).toBeUndefined();
});
