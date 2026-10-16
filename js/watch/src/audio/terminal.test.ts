import { expect, test } from "bun:test";
import type { Micro } from "@moq/clock";
import { Terminal } from "./terminal";

const frame = (group: number, timestamp: number) => ({
	kind: "frame" as const,
	group,
	frame: { timestamp: timestamp as Micro },
});
const end = (group: number, timestamp: number) => ({ kind: "end" as const, group, end: timestamp as Micro });
const gap = { kind: "gap" as const };

test("warmup trims by timestamp across callback sizes, not ordinary groups or declared returns", () => {
	const terminal = new Terminal();
	terminal.clear(0, 80_000 as Micro);
	terminal.update(frame(10, 1_000_000));
	expect(terminal.span({ timestamp: 1_000_000, sampleRate: 48_000, numberOfFrames: 2_880 }).frames).toBe(0);
	terminal.update(frame(11, 1_060_000));
	expect(terminal.span({ timestamp: 1_060_000, sampleRate: 48_000, numberOfFrames: 2_880 })).toEqual({
		timestamp: 1_080_000 as Micro,
		frameOffset: 960,
		frames: 1_920,
	});
	terminal.update(frame(12, 1_120_000));
	expect(terminal.span({ timestamp: 1_120_000, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(960);
	terminal.update(end(13, 1_140_000));
	terminal.update(frame(14, 2_000_000));
	expect(terminal.span({ timestamp: 2_000_000, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(960);
	terminal.update(gap);
	terminal.update(frame(16, 3_000_000));
	expect(terminal.span({ timestamp: 3_000_000, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(0);
});

test("Terminal trims a partial packet to the source endpoint", () => {
	const terminal = new Terminal();
	terminal.update(end(0, 17_916));
	terminal.update(frame(0, 0));
	expect(terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(860);
});

test("Terminal maps Opus delay back before trimming terminal output", () => {
	const terminal = new Terminal();
	terminal.clear(312);
	terminal.update(frame(0, 0));
	const body = terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 });
	expect(body).toEqual({ timestamp: 0 as Micro, frameOffset: 312, frames: 648 });

	expect(terminal.update(end(1, 20_000))).toBe(false);
	// The drain shares the end's group, so it continues the epoch.
	expect(terminal.update(frame(1, 20_000))).toBe(false);
	const drain = terminal.span({ timestamp: 20_000, sampleRate: 48_000, numberOfFrames: 960 });
	expect(drain).toEqual({ timestamp: 13_500 as Micro, frameOffset: 0, frames: 312 });
	expect(body.frames + drain.frames).toBe(960);
});

test("Terminal preserves samples before the source endpoint", () => {
	const terminal = new Terminal();
	terminal.update(end(0, 20_000));
	terminal.update(frame(0, 0));
	expect(terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(960);
});

test("a gap before any frame ends no epoch", () => {
	const terminal = new Terminal();
	expect(terminal.update(gap)).toBe(false);
});

test("an endpoint after a gap trims its own group", () => {
	const terminal = new Terminal();
	terminal.clear(312);
	terminal.update(frame(0, 0));
	expect(terminal.update(gap)).toBe(true);

	terminal.update(end(1, 20_000));
	expect(terminal.end).toBe(20_000 as Micro);
	terminal.update(frame(1, 20_000));
	const span = terminal.span({ timestamp: 20_000, sampleRate: 48_000, numberOfFrames: 960 });
	expect(span).toEqual({ timestamp: 20_000 as Micro, frameOffset: 312, frames: 0 });
});

test("a gap reapplies Opus pre-skip in the resumed epoch", () => {
	const terminal = new Terminal();
	terminal.clear(312);
	terminal.update(frame(0, 0));
	expect(terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }).frameOffset).toBe(312);

	expect(terminal.update(gap)).toBe(true);
	terminal.update(frame(1, 1_000_000));
	const resumed = terminal.span({ timestamp: 1_000_000, sampleRate: 48_000, numberOfFrames: 960 });
	expect(resumed).toEqual({ timestamp: 1_000_000 as Micro, frameOffset: 312, frames: 648 });
});

test("an endpoint does not trim media resumed in a later group", () => {
	const terminal = new Terminal();
	terminal.update(frame(0, 0));
	// A discontinuity marker: its own group, ending the epoch at the live edge.
	expect(terminal.update(end(1, 20_000))).toBe(false);
	// The next group comes from a fresh encoder.
	expect(terminal.update(frame(2, 5_000_000))).toBe(true);
	expect(terminal.end).toBeUndefined();
	expect(terminal.span({ timestamp: 5_000_000, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(960);
});
