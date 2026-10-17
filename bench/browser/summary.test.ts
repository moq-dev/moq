import { expect, test } from "bun:test";
import { compare, type Row, range } from "./summary.ts";

const row = (variant: string, round: number, value: number): Row => ({
	scenario: "transport",
	key: "transport 160B x10",
	shape: {},
	variant,
	round,
	pass: "timed",
	delivered: { frames: 10, bytes: 1600, digest: 0 },
	metrics: { cpu: value },
	heap: { before: null, after: null },
	load: [],
});

test("range takes the middle pair's mean for an even count", () => {
	expect(range([4, 1, 3, 2])).toEqual({ min: 1, median: 2.5, max: 4 });
});

test("compare pairs each variant with the reference in the same round", () => {
	const rows = [row("current", 0, 100), row("copy", 0, 110), row("current", 1, 200), row("copy", 1, 230)];
	const [entry] = compare(rows, "current");
	expect(entry).toMatchObject({
		variant: "copy",
		metric: "cpu",
		pairs: 2,
		min_pct: 10,
		max_pct: 15,
		consistent: true,
	});
	expect(entry.median_pct).toBeCloseTo(12.5);
});

test("compare flags a sign that varies across pairs", () => {
	const rows = [row("current", 0, 100), row("aa", 0, 105), row("current", 1, 100), row("aa", 1, 95)];
	expect(compare(rows, "current")[0].consistent).toBe(false);
});
