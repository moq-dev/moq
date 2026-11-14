import { expect, test } from "bun:test";
import fc from "fast-check";
import { Sorted } from "./sorted.ts";

type Op = { insert: boolean; key: number };

// Enough distinct keys and operations to split, merge, and empty chunks of 256.
const ops = fc.array(fc.record({ insert: fc.boolean(), key: fc.integer({ min: 0, max: 3000 }) }), {
	maxLength: 4000,
	size: "max",
});

test("agrees with a sorted array on every lookup after any inserts and deletes", () => {
	fc.assert(
		fc.property(ops, fc.integer({ min: -1, max: 3001 }), (ops: Op[], probe) => {
			const sorted = new Sorted<number, { key: number }>(
				(value) => value.key,
				(a, b) => a - b,
			);
			const model = new Set<number>();
			for (const { insert, key } of ops) {
				if (insert && !model.has(key)) {
					sorted.insert({ key });
					model.add(key);
				} else if (!insert) {
					sorted.delete(key);
					model.delete(key);
				}
			}
			const keys = [...model].sort((a, b) => a - b);
			expect(sorted.size).toBe(keys.length);
			expect([...sorted.from(-1)].map((value) => value.key)).toEqual(keys);
			expect(sorted.first()?.key).toBe(keys[0]);
			expect(sorted.last()?.key).toBe(keys.at(-1));
			for (const key of [probe, ...keys.slice(0, 50)]) {
				expect(sorted.ceil(key)?.key).toBe(keys.find((k) => k >= key));
				expect(sorted.higher(key)?.key).toBe(keys.find((k) => k > key));
				expect(sorted.floor(key)?.key).toBe(keys.findLast((k) => k <= key));
				expect(sorted.lower(key)?.key).toBe(keys.findLast((k) => k < key));
				expect([...sorted.from(key)].map((value) => value.key)).toEqual(keys.filter((k) => k >= key));
			}
		}),
		{ numRuns: 100 },
	);
});

test("draining in any order empties every chunk", () => {
	const sorted = new Sorted<number, number>(
		(value) => value,
		(a, b) => a - b,
	);
	for (let i = 0; i < 5000; i++) sorted.insert((i * 7919) % 5000);
	for (let i = 0; i < 5000; i++) sorted.delete((i * 104729) % 5000);
	expect(sorted.size).toBe(0);
	expect(sorted.first()).toBeUndefined();
	expect([...sorted.from(0)]).toEqual([]);
});
