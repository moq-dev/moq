import { describe, expect, test } from "bun:test";
import { Time } from "@moq/net";
import { Pacer } from "./polyfill";

// Wake once per display refresh, with the clock clamped to 1ms like Safari and Firefox.
function capture(frameRate: number, seconds: number, displayHz = 60): number {
	const pacer = new Pacer(frameRate, Time.Milli(0));

	let frames = 0;
	const ticks = Math.round(seconds * displayHz);
	for (let i = 1; i <= ticks; i++) {
		const now = Time.Milli(Math.floor((i * 1000) / displayHz));
		if (pacer.due(now)) frames++;
	}

	return frames / seconds;
}

describe("Pacer", () => {
	test("captures the source frame rate on a clamped clock", () => {
		// A 60Hz display and a 30fps camera is the case from #2208: the frame period is 33.333ms, the
		// clamped clock reports the two elapsed animation frames as 33, and comparing against the wake
		// time then waits for a third frame, capturing every 50ms instead of every 33ms.
		expect(capture(30, 10)).toBeCloseTo(30, 0);
		expect(capture(24, 10)).toBeCloseTo(24, 0);
		expect(capture(60, 10)).toBeCloseTo(60, 0);
	});

	test("never captures faster than the display", () => {
		expect(capture(120, 10)).toBeCloseTo(60, 0);
	});

	test("resyncs after a stall instead of capturing a burst", () => {
		const pacer = new Pacer(30, Time.Milli(0));
		expect(pacer.due(Time.Milli(0))).toBe(true);

		// The tab was hidden for a second, which suspends requestAnimationFrame entirely.
		expect(pacer.due(Time.Milli(1000))).toBe(true);

		// The next frame is one period out, not a second's worth of backlog.
		expect(pacer.due(Time.Milli(1001))).toBe(false);
		expect(pacer.due(Time.Milli(1034))).toBe(true);
	});
});
