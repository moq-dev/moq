import { expect, test } from "bun:test";
import * as Epoch from "./epoch.ts";

test("mint parses as canonical and orders successive identities", () => {
	const epoch = Epoch.mint();
	expect(Epoch.parse(epoch)).toBe(epoch);
	expect(Epoch.mint() > epoch).toBe(true);
});

// Shared with rs/moq-e2ee's `epoch_vectors`, so both parse and order alike.
const vectors = (await Bun.file(new URL("../../../rs/moq-e2ee/src/epoch.json", import.meta.url)).json()) as {
	valid: string[];
	invalid: string[];
	ordered: string[];
};

test("shared epoch parse, reject, and order vectors", () => {
	for (const text of vectors.valid) {
		const epoch = Epoch.parse(text);
		expect(epoch).toBe(text as Epoch.Valid);
	}
	for (const text of vectors.invalid) expect(() => Epoch.parse(text)).toThrow(RangeError);
	const ordered = vectors.ordered.map(Epoch.parse);
	expect([...ordered].reverse().sort()).toEqual(ordered);
});
