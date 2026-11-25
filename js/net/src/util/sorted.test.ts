import { expect, test } from "bun:test";
import fc from "fast-check";
import { Sorted } from "./sorted.ts";

type Op = { insert: boolean; key: number };

// Fill, delete a random share in random order, then churn: enough distinct keys and operations
// to split, merge, and empty chunks of 256.
const ops = fc
	.uniqueArray(fc.integer({ min: 0, max: 3000 }), { maxLength: 1500, size: "max" })
	.chain((fill) =>
		fc.tuple(
			fc.constant(fill),
			fc.shuffledSubarray(fill),
			fc.array(fc.record({ insert: fc.boolean(), key: fc.integer({ min: 0, max: 3000 }) }), { maxLength: 500 }),
		),
	)
	.map(([fill, drain, churn]): Op[] => [
		...fill.map((key) => ({ insert: true, key })),
		...drain.map((key) => ({ insert: false, key })),
		...churn,
	]);

// The first index whose key is above `key`, or at or above it when `inclusive`.
function bound(keys: number[], key: number, inclusive: boolean): number {
	let lo = 0;
	let hi = keys.length;
	while (lo < hi) {
		const mid = (lo + hi) >>> 1;
		if (inclusive ? keys[mid] >= key : keys[mid] > key) hi = mid;
		else lo = mid + 1;
	}
	return lo;
}

// The model is a sorted array of keys, so a lookup is a bisection rather than a scan.
test("agrees with a sorted array on every lookup after any inserts and deletes", () => {
	fc.assert(
		fc.property(ops, fc.integer({ min: -1, max: 3001 }), (ops: Op[], probe) => {
			const sorted = new Sorted<number, { key: number }>(
				(value) => value.key,
				(a, b) => a - b,
			);
			const keys: number[] = [];
			for (const { insert, key } of ops) {
				const at = bound(keys, key, true);
				const present = keys[at] === key;
				if (insert && !present) {
					sorted.insert({ key });
					keys.splice(at, 0, key);
				} else if (!insert) {
					sorted.delete(key);
					if (present) keys.splice(at, 1);
				}
			}
			expect(sorted.size).toBe(keys.length);
			expect([...sorted.from(-1)].map((value) => value.key)).toEqual(keys);
			expect(sorted.first()?.key).toBe(keys[0]);
			expect(sorted.last()?.key).toBe(keys.at(-1));
			// The probe plus held keys spread across every chunk.
			const stride = Math.max(1, keys.length >>> 4);
			const probes = [probe];
			for (let i = 0; i < keys.length; i += stride) probes.push(keys[i]);
			for (const key of probes) {
				const lo = bound(keys, key, true);
				const hi = bound(keys, key, false);
				expect(sorted.ceil(key)?.key).toBe(keys[lo]);
				expect(sorted.higher(key)?.key).toBe(keys[hi]);
				expect(sorted.floor(key)?.key).toBe(keys[hi - 1]);
				expect(sorted.lower(key)?.key).toBe(keys[lo - 1]);
				expect([...sorted.from(key)].map((value) => value.key)).toEqual(keys.slice(lo));
			}
		}),
		{ numRuns: 30 },
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
