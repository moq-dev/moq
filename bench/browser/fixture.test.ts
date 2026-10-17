import { expect, test } from "bun:test";
import { check, digest, payload } from "./fixture.ts";

test("a complete stream matches its digest in any arrival order", () => {
	const frames = [3, 0, 2, 1].map((frame) => check(payload(1200, frame), 1200, frame, true));
	expect(frames.reduce((sum, hash) => (sum + hash) >>> 0, 0)).toBe(digest(1200, 4));
});

test("a frame delivered under the wrong index fails", () => {
	expect(() => check(payload(160, 1), 160, 2, false)).toThrow("carries index 1");
});

test("a corrupted byte fails the full check even when no probe lands on it", () => {
	const bytes = payload(4096, 0);
	bytes[5] ^= 1;
	expect(() => check(bytes, 4096, 0, false)).not.toThrow();
	expect(() => check(bytes, 4096, 0, true)).toThrow("incorrect byte 5");
});
