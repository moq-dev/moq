import { describe, expect, it } from "bun:test";
import { snapTimestamp } from "./snap";

const THRESHOLD = 5000; // us, matches SNAP_US in decoder.ts

describe("snapTimestamp", () => {
	it("snaps a near-contiguous timestamp to the expected value", () => {
		expect(snapTimestamp(20000, 20002, THRESHOLD)).toBe(20000);
		expect(snapTimestamp(20000, 19998, THRESHOLD)).toBe(20000);
		expect(snapTimestamp(20000, 20000, THRESHOLD)).toBe(20000);
		// Exactly at the threshold still snaps.
		expect(snapTimestamp(20000, 25000, THRESHOLD)).toBe(20000);
	});

	it("passes through a genuine gap (beyond the threshold)", () => {
		expect(snapTimestamp(20000, 40000, THRESHOLD)).toBe(40000);
		expect(snapTimestamp(20000, 25001, THRESHOLD)).toBe(25001);
	});

	it("passes through when there is no expectation yet", () => {
		expect(snapTimestamp(undefined, 12345, THRESHOLD)).toBe(12345);
	});

	it("caps the window at half a frame so a real gap is never snapped (adaptive window in #emit)", () => {
		// #emit uses Math.min(SNAP_US, durationMicro / 2). For an exotic 2.5 ms frame that window is 1250 us,
		// smaller than SNAP_US, so a one-frame gap (2500 us) must pass through instead of being snapped shut.
		const durationMicro = 2500;
		const window = Math.min(THRESHOLD, durationMicro / 2);
		expect(window).toBe(1250);
		// Sub-window jitter still snaps; a full-frame gap does not.
		expect(snapTimestamp(2500, 3000, window)).toBe(2500);
		expect(snapTimestamp(2500, 5000, window)).toBe(5000);
	});
});
