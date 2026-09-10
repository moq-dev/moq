import { expect, test } from "bun:test";
import type { Time } from "@moq/net";
import { Terminal } from "./terminal";

test("Terminal maps Opus delay back onto the source timeline", () => {
	const terminal = new Terminal();
	terminal.clear(312);
	terminal.update({ discontinuity: 0, frame: { timestamp: 0 as Time.Micro } });
	const body = terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 });
	expect(body).toEqual({ timestamp: 0 as Time.Micro, frameOffset: 312, frames: 648 });
});

test("a discontinuity reapplies Opus pre-skip in the resumed epoch", () => {
	const terminal = new Terminal();
	terminal.clear(312);
	terminal.update({ discontinuity: 0, frame: { timestamp: 0 as Time.Micro } });
	expect(terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }).frameOffset).toBe(312);

	terminal.update({ discontinuity: 1, frame: { timestamp: 1_000_000 as Time.Micro } });
	const resumed = terminal.span({ timestamp: 1_000_000, sampleRate: 48_000, numberOfFrames: 960 });
	expect(resumed).toEqual({ timestamp: 1_000_000 as Time.Micro, frameOffset: 312, frames: 648 });
});
