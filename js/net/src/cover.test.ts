import { expect, test } from "bun:test";
import { Cover } from "./cover.ts";

// `prev` finds the highest covered key below a bound in one descent; checked against the runs it
// would otherwise walk, over random overlapping and cancelling edits.
test("prev agrees with the runs below it", () => {
	let seed = 1;
	const random = (below: number) => {
		seed = (seed * 1103515245 + 12345) % 2 ** 31;
		return seed % below;
	};
	for (let round = 0; round < 200; round++) {
		const cover = new Cover();
		const added: [bigint, bigint][] = [];
		for (let edit = 0; edit < 12; edit++) {
			if (added.length > 0 && random(3) === 0) {
				const [start, end] = added.splice(random(added.length), 1)[0];
				cover.add(start, end, -1);
				continue;
			}
			const start = BigInt(random(60));
			const end = start + BigInt(1 + random(10));
			cover.add(start, end, 1);
			added.push([start, end]);
		}
		for (let before = 0n; before <= 72n; before++) {
			const expected = cover.runs(0n, before).at(-1);
			expect(cover.prev(before)).toBe(expected === undefined ? undefined : expected.end - 1n);
		}
	}
});
