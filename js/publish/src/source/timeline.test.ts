import { describe, expect, test } from "bun:test";
import type { Time } from "@moq/net";
import { Timeline } from "./timeline";

// Time.Micro is branded, so a bare number literal isn't assignable to it.
const micro = (value: number): Time.Micro => value as Time.Micro;

const ZERO = micro(1_000_000);

describe("Timeline", () => {
	test("offsets file timestamps onto the wall clock", () => {
		const timeline = new Timeline(ZERO, 10);

		expect(timeline.at(micro(0), 0)).toBe(ZERO);
		expect(timeline.at(micro(2_500_000), 0)).toBe(micro(3_500_000));
	});

	test("shifts each loop by the file duration", () => {
		const timeline = new Timeline(ZERO, 10);

		expect(timeline.at(micro(0), 1)).toBe(micro(11_000_000));
		expect(timeline.at(micro(0), 3)).toBe(micro(31_000_000));
		expect(timeline.at(micro(4_000_000), 2)).toBe(micro(25_000_000));
	});

	// The encoder rejects a timestamp that goes backwards, so the wrap has to stay monotonic even
	// though the file's own timestamps reset to zero on every pass.
	test("stays monotonic across a wrap", () => {
		const timeline = new Timeline(ZERO, 10);

		const endOfLoop = timeline.at(micro(9_900_000), 0);
		const startOfNext = timeline.at(micro(0), 1);

		expect(startOfNext).toBeGreaterThan(endOfLoop);
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
		expect(timeline.at(micro(5_000_000), 0)).toBe(micro(6_000_000));
	});

	test("loops on a usable duration", () => {
		expect(new Timeline(ZERO, 10).loops).toBe(true);
	});
});
