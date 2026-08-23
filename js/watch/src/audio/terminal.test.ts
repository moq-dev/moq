import { expect, test } from "bun:test";
import type { Time } from "@moq/net";
import { Terminal, terminalFrames } from "./terminal";

test("terminalFrames trims a partial Opus packet to the source endpoint", () => {
	const frames = terminalFrames({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }, 17_916 as Time.Micro);
	expect(frames).toBe(860);
});

test("terminalFrames drops drain output starting at the source endpoint", () => {
	const frames = terminalFrames({ timestamp: 20_000, sampleRate: 48_000, numberOfFrames: 960 }, 20_000 as Time.Micro);
	expect(frames).toBe(0);
});

test("terminalFrames preserves samples before the source endpoint", () => {
	const frames = terminalFrames({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }, 20_000 as Time.Micro);
	expect(frames).toBe(960);
});

test("a rewound endpoint survives the discontinuity reset and trims its terminal packet", () => {
	const terminal = new Terminal();
	terminal.update({ discontinuity: 0, end: 40_000 as Time.Micro });

	const reset = terminal.update({ discontinuity: 1, end: 20_000 as Time.Micro });
	expect(reset).toBe(true);
	expect(terminal.end).toBe(20_000 as Time.Micro);

	const frames = terminalFrames({ timestamp: 20_000, sampleRate: 48_000, numberOfFrames: 960 }, terminal.end);
	expect(frames).toBe(0);
});
