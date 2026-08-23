import { expect, test } from "bun:test";
import type { Time } from "@moq/net";
import { terminalFrames } from "./terminal";

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
