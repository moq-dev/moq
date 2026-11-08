import { describe, expect, test } from "bun:test";
import { Duration, Instant } from "@moq/clock";
import { Timeline } from "./timeline";

const micro = (value: number): Duration => Duration.micros(value);
// A point on the clock, `value` microseconds past its origin.
const at = (value: number): Instant => Instant.ZERO.add(micro(value));

const ZERO = at(1_000_000);

describe("Timeline", () => {
	test("offsets file timestamps onto the wall clock", () => {
		const timeline = new Timeline(ZERO, 10);

		expect(timeline.at(micro(0), 0)).toEqual(ZERO);
		expect(timeline.at(micro(2_500_000), 0)).toEqual(at(3_500_000));
	});

	test("shifts each loop by the file duration", () => {
		const timeline = new Timeline(ZERO, 10);

		expect(timeline.at(micro(0), 1)).toEqual(at(11_000_000));
		expect(timeline.at(micro(0), 3)).toEqual(at(31_000_000));
		expect(timeline.at(micro(4_000_000), 2)).toEqual(at(25_000_000));
	});

	// The encoder rejects a timestamp that goes backwards, so the wrap has to stay monotonic even
	// though the file's own timestamps reset to zero on every pass.
	test("stays monotonic across a wrap", () => {
		const timeline = new Timeline(ZERO, 10);

		const endOfLoop = timeline.at(micro(9_900_000), 0);
		const startOfNext = timeline.at(micro(0), 1);

		expect(startOfNext.gt(endOfLoop)).toBe(true);
	});

	// A duration we can't trust would make every loop land on top of the last one, so play once.
	test.each([
		["zero", 0],
		["negative", -1],
		["infinite", Number.POSITIVE_INFINITY],
		["unknown", Number.NaN],
	])("does not loop on a %s duration", (_name, duration) => {
		const timeline = new Timeline(ZERO, duration);

		expect(timeline.loops).toBe(false);
		expect(timeline.at(micro(5_000_000), 0)).toEqual(at(6_000_000));
	});

	test("loops on a usable duration", () => {
		expect(new Timeline(ZERO, 10).loops).toBe(true);
	});

	test("maps a wall clock time back to its pass and file timestamp", () => {
		const timeline = new Timeline(ZERO, 10);

		expect(timeline.position(at(500_000))).toEqual({ timestamp: micro(0), loop: 0 });
		expect(timeline.position(at(3_500_000))).toEqual({ timestamp: micro(2_500_000), loop: 0 });
		expect(timeline.position(at(26_000_000))).toEqual({ timestamp: micro(5_000_000), loop: 2 });
		expect(timeline.position(timeline.at(micro(4_000_000), 3))).toEqual({ timestamp: micro(4_000_000), loop: 3 });
		expect(new Timeline(ZERO, 0).position(at(26_000_000))).toEqual({ timestamp: micro(25_000_000), loop: 0 });
	});
});
