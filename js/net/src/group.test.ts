import { expect, test } from "bun:test";
import { Group } from "./group.ts";

const dec = new TextDecoder();

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
