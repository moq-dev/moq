import { expect, test } from "bun:test";
import { diff, merge } from "./diff.ts";

// Shared cross-impl fixture, owned by the reference Rust crate. Both suites assert the same
// vectors so the two implementations agree on every snapshot/delta decision and patch shape.
const url = new URL("../../../rs/moq-json/tests/vectors.json", import.meta.url);
const vectors = (await Bun.file(url).json()) as Array<{
	name: string;
	old: unknown;
	new: unknown;
	forced: boolean;
	patch?: unknown;
}>;

for (const vector of vectors) {
	test(`vector: ${vector.name}`, () => {
		const result = diff(vector.old, vector.new);
		expect(result.forcedSnapshot).toBe(vector.forced);

		if (!vector.forced) {
			expect(result.patch).toEqual(vector.patch as object);
			expect(merge(vector.old, result.patch)).toEqual(vector.new as object);
		}
	});
}
