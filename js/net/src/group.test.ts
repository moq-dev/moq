import { expect, test } from "bun:test";
import { Group, MAX_GROUP_CACHE_BYTES, MAX_GROUP_FRAMES } from "./group.ts";

test("a group caps its frame count, dropping from the front", () => {
	const group = new Group(0);

	const extra = 100;
	for (let i = 0; i < MAX_GROUP_FRAMES + extra; i++) {
		group.writeFrame(new Uint8Array([i & 0xff]));
	}

	const frames = group.state.frames.peek();
	expect(frames.length).toBe(MAX_GROUP_FRAMES);
	// `total` still counts every frame written, so frame indices stay consistent.
	expect(group.state.total.peek()).toBe(MAX_GROUP_FRAMES + extra);
	// The oldest `extra` frames were dropped: the front is now frame `extra`.
	expect(frames[0][0]).toBe(extra & 0xff);
});

test("a group caps its byte size, dropping from the front", () => {
	const group = new Group(0);

	// 40 x 1 MiB = 40 MiB, over the 32 MiB cap.
	const oneMiB = 1024 * 1024;
	for (let i = 0; i < 40; i++) {
		group.writeFrame(new Uint8Array(oneMiB));
	}

	const frames = group.state.frames.peek();
	const bytes = frames.reduce((sum, f) => sum + f.byteLength, 0);
	expect(bytes).toBeLessThanOrEqual(MAX_GROUP_CACHE_BYTES);
	expect(frames.length).toBe(MAX_GROUP_CACHE_BYTES / oneMiB);
});
