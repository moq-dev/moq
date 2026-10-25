import { expect, test } from "bun:test";
import * as Epoch from "./epoch.ts";

test("mint parses as canonical and orders successive identities", () => {
	const epoch = Epoch.mint();
	expect(Epoch.parse(epoch)).toBe(epoch);
	expect(Epoch.mint() > epoch).toBe(true);
});

// Shared with rs/moq-net's `epoch_vectors`, so both parse and order alike.
const vectors = (await Bun.file(new URL("../../../rs/moq-net/src/epoch.json", import.meta.url)).json()) as {
	valid: string[];
	invalid: string[];
	ordered: string[];
};

test("shared epoch parse, reject, and order vectors", () => {
	for (const text of vectors.valid) {
		const epoch = Epoch.parse(text);
		expect(epoch).toBe(text as Epoch.Valid);
		expect(Epoch.fromBytes(Epoch.toBytes(epoch))).toBe(epoch);
	}
	for (const text of vectors.invalid) expect(() => Epoch.parse(text)).toThrow(RangeError);
	const ordered = vectors.ordered.map(Epoch.parse);
	expect([...ordered].reverse().sort()).toEqual(ordered);
});

test("fromBytes refuses anything but 16 bytes of UUIDv7", () => {
	expect(() => Epoch.fromBytes(new Uint8Array(15))).toThrow(RangeError);
	const v4 = Epoch.toBytes(Epoch.mint());
	v4[6] = (v4[6] & 0x0f) | 0x40;
	expect(() => Epoch.fromBytes(v4)).toThrow(RangeError);
});
