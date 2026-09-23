import { expect, test } from "bun:test";
import type { Time } from "@moq/net";
import { Terminal } from "./terminal";

test("Terminal trims a partial packet to the source endpoint", () => {
	const terminal = new Terminal();
	terminal.update({ discontinuity: 0, group: 0, frame: { timestamp: 0 as Time.Micro }, end: 17_916 as Time.Micro });
	expect(terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(860);
});

test("Terminal maps Opus delay back before trimming terminal output", () => {
	const terminal = new Terminal();
	terminal.clear(312);
	terminal.update({ discontinuity: 0, group: 0, frame: { timestamp: 0 as Time.Micro } });
	const body = terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 });
	expect(body).toEqual({ timestamp: 0 as Time.Micro, frameOffset: 312, frames: 648 });

	terminal.update({ discontinuity: 0, group: 0, end: 20_000 as Time.Micro });
	terminal.update({ discontinuity: 0, group: 0, frame: { timestamp: 20_000 as Time.Micro } });
	const drain = terminal.span({ timestamp: 20_000, sampleRate: 48_000, numberOfFrames: 960 });
	expect(drain).toEqual({ timestamp: 13_500 as Time.Micro, frameOffset: 0, frames: 312 });
	expect(body.frames + drain.frames).toBe(960);
});

test("Terminal preserves samples before the source endpoint", () => {
	const terminal = new Terminal();
	terminal.update({ discontinuity: 0, group: 0, frame: { timestamp: 0 as Time.Micro }, end: 20_000 as Time.Micro });
	expect(terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(960);
});

test("a rewound endpoint survives the discontinuity reset and trims its terminal packet", () => {
	const terminal = new Terminal();
	terminal.clear(312);
	terminal.update({ discontinuity: 0, group: 0, end: 40_000 as Time.Micro });

	const reset = terminal.update({ discontinuity: 1, group: 1, end: 20_000 as Time.Micro });
	expect(reset).toBe(true);
	expect(terminal.end).toBe(20_000 as Time.Micro);

	terminal.update({ discontinuity: 1, group: 1, frame: { timestamp: 20_000 as Time.Micro } });
	const span = terminal.span({ timestamp: 20_000, sampleRate: 48_000, numberOfFrames: 960 });
	expect(span).toEqual({ timestamp: 20_000 as Time.Micro, frameOffset: 312, frames: 0 });
});

test("a discontinuity reapplies Opus pre-skip in the resumed epoch", () => {
	const terminal = new Terminal();
	terminal.clear(312);
	terminal.update({ discontinuity: 0, group: 0, frame: { timestamp: 0 as Time.Micro } });
	expect(terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }).frameOffset).toBe(312);

	terminal.update({ discontinuity: 1, group: 1, frame: { timestamp: 1_000_000 as Time.Micro } });
	const resumed = terminal.span({ timestamp: 1_000_000, sampleRate: 48_000, numberOfFrames: 960 });
	expect(resumed).toEqual({ timestamp: 1_000_000 as Time.Micro, frameOffset: 312, frames: 648 });
});

test("an endpoint does not trim media resumed in a later group", () => {
	const terminal = new Terminal();
	terminal.update({ discontinuity: 0, group: 0, frame: { timestamp: 0 as Time.Micro } });
	// A discontinuity marker: its own group, ending the epoch at the live edge.
	terminal.update({ discontinuity: 1, group: 1, end: 20_000 as Time.Micro });
	terminal.update({ discontinuity: 1, group: 2, frame: { timestamp: 5_000_000 as Time.Micro } });
	expect(terminal.end).toBeUndefined();
	expect(terminal.span({ timestamp: 5_000_000, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(960);
});
