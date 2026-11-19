import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Terminal } from "./terminal";

const frame = (group: bigint, timestamp: number) => ({
	kind: "frame" as const,
	group,
	frame: { timestamp: Duration.micros(timestamp) },
});
const end = (group: bigint, timestamp: number) => ({ kind: "end" as const, group, end: Duration.micros(timestamp) });
const gap = { kind: "gap" as const };

test("warmup trims by timestamp across callback sizes, not ordinary groups or declared returns", () => {
	const terminal = new Terminal();
	terminal.clear(0, 80_000);
	terminal.update(frame(10n, 1_000_000));
	expect(terminal.span({ timestamp: 1_000_000, sampleRate: 48_000, numberOfFrames: 2_880 }).frames).toBe(0);
	terminal.update(frame(11n, 1_060_000));
	expect(terminal.span({ timestamp: 1_060_000, sampleRate: 48_000, numberOfFrames: 2_880 })).toEqual({
		timestamp: 1_080_000,
		frameOffset: 960,
		frames: 1_920,
	});
	terminal.update(frame(12n, 1_120_000));
	expect(terminal.span({ timestamp: 1_120_000, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(960);
	terminal.update(end(13n, 1_140_000));
	terminal.update(frame(14n, 2_000_000));
	expect(terminal.span({ timestamp: 2_000_000, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(960);
	terminal.update(gap);
	terminal.update(frame(16n, 3_000_000));
	expect(terminal.span({ timestamp: 3_000_000, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(0);
});

test("Terminal trims a partial packet to the source endpoint", () => {
	const terminal = new Terminal();
	terminal.update(end(0n, 17_916));
	terminal.update(frame(0n, 0));
	expect(terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(860);
});

test("Terminal maps Opus delay back before trimming terminal output", () => {
	const terminal = new Terminal();
	terminal.clear(312);
	terminal.update(frame(0n, 0));
	const body = terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 });
	expect(body).toEqual({ timestamp: 0, frameOffset: 312, frames: 648 });

	expect(terminal.update(end(1n, 20_000))).toBe(false);
	// The drain shares the end's group, so it continues the epoch.
	expect(terminal.update(frame(1n, 20_000))).toBe(false);
	const drain = terminal.span({ timestamp: 20_000, sampleRate: 48_000, numberOfFrames: 960 });
	expect(drain).toEqual({ timestamp: 13_500, frameOffset: 0, frames: 312 });
	expect(body.frames + drain.frames).toBe(960);
});

test("Terminal preserves samples before the source endpoint", () => {
	const terminal = new Terminal();
	terminal.update(end(0n, 20_000));
	terminal.update(frame(0n, 0));
	expect(terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(960);
});

test("a gap before any frame ends no epoch", () => {
	const terminal = new Terminal();
	expect(terminal.update(gap)).toBe(false);
});

test("an endpoint after a gap trims its own group", () => {
	const terminal = new Terminal();
	terminal.clear(312);
	terminal.update(frame(0n, 0));
	expect(terminal.update(gap)).toBe(true);

	terminal.update(end(1n, 20_000));
	expect(terminal.end).toBe(20_000);
	terminal.update(frame(1n, 20_000));
	const span = terminal.span({ timestamp: 20_000, sampleRate: 48_000, numberOfFrames: 960 });
	expect(span).toEqual({ timestamp: 20_000, frameOffset: 312, frames: 0 });
});

test("a gap reapplies Opus pre-skip in the resumed epoch", () => {
	const terminal = new Terminal();
	terminal.clear(312);
	terminal.update(frame(0n, 0));
	expect(terminal.span({ timestamp: 0, sampleRate: 48_000, numberOfFrames: 960 }).frameOffset).toBe(312);

	expect(terminal.update(gap)).toBe(true);
	terminal.update(frame(1n, 1_000_000));
	const resumed = terminal.span({ timestamp: 1_000_000, sampleRate: 48_000, numberOfFrames: 960 });
	expect(resumed).toEqual({ timestamp: 1_000_000, frameOffset: 312, frames: 648 });
});

test("an endpoint does not trim media resumed in a later group", () => {
	const terminal = new Terminal();
	terminal.update(frame(0n, 0));
	// The group ended whole and nothing continued it, which ends the epoch at its end.
	expect(terminal.update(end(0n, 20_000))).toBe(false);
	// The next group comes from a fresh encoder.
	expect(terminal.update(frame(2n, 5_000_000))).toBe(true);
	expect(terminal.end).toBeUndefined();
	expect(terminal.span({ timestamp: 5_000_000, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(960);
});

test("a hole preserves a declared endpoint until a later group starts a new timeline", () => {
	const terminal = new Terminal();
	terminal.update(frame(0n, 0));
	terminal.update(end(1n, 20_000));
	expect(terminal.update(gap)).toBe(true);
	expect(terminal.end).toBe(20_000);
	expect(terminal.update(frame(1n, 10_000))).toBe(false);
	expect(terminal.span({ timestamp: 10_000, sampleRate: 48_000, numberOfFrames: 960 }).frames).toBe(480);
	expect(terminal.update(frame(2n, 1_000_000))).toBe(true);
	expect(terminal.end).toBeUndefined();
});

const packet = (timestamp: number) => ({
	timestamp: Duration.micros(timestamp),
	payload: new Uint8Array([timestamp / 20_000]),
});
const opus = (timestamp: number) => ({ timestamp, sampleRate: 48_000, numberOfFrames: 960 });

test("a gap re-primes the decoder on the warmup before it, so the audio after it plays whole", () => {
	const terminal = new Terminal();
	terminal.clear(312, 80_000);
	for (let i = 0; i < 6; i++) {
		terminal.update(frame(0n, i * 20_000));
		terminal.decoded(packet(i * 20_000), Duration.micros(20_000));
	}
	expect(terminal.update(gap)).toBe(true);
	terminal.update(frame(2n, 1_000_000));

	// The newest 80 ms, back to back and ending where the frame after the gap starts.
	const preroll = terminal.preroll();
	expect(preroll.map((p) => p.timestamp)).toEqual([920_000, 940_000, 960_000, 980_000]);
	expect(preroll.map((p) => p.payload[0])).toEqual([2, 3, 4, 5]);
	expect(terminal.preroll()).toEqual([]);

	// The pre-roll's output takes the pre-skip and the warmup, and answers no frame.
	for (const { timestamp } of preroll) {
		expect(terminal.primes(opus(timestamp))).toBe(true);
		expect(terminal.span(opus(timestamp)).frames).toBe(0);
	}
	// The overlap the pre-skip carries over the restart starts before the frame, so only it is trimmed.
	expect(terminal.primes(opus(1_000_000))).toBe(false);
	expect(terminal.span(opus(1_000_000))).toEqual({ timestamp: 1_000_000, frameOffset: 312, frames: 648 });
	expect(terminal.span(opus(1_020_000))).toEqual({ timestamp: 1_013_500, frameOffset: 0, frames: 960 });
});

// A decoder that coalesces outputs can hand back the pre-roll and the frame after the gap together.
test("output reaching past the pre-roll holds the audio after the gap", () => {
	const terminal = new Terminal();
	terminal.clear(0, 80_000);
	for (let i = 0; i < 4; i++) {
		terminal.update(frame(0n, i * 20_000));
		terminal.decoded(packet(i * 20_000), Duration.micros(20_000));
	}
	terminal.update(gap);
	terminal.update(frame(1n, 1_000_000));
	expect(terminal.preroll()).toHaveLength(4);

	// 20 ms of pre-roll alone, then the other 60 ms of it with the frame after the gap.
	expect(terminal.primes(opus(920_000))).toBe(true);
	const coalesced = { timestamp: 940_000, sampleRate: 48_000, numberOfFrames: 4 * 960 };
	expect(terminal.primes(coalesced)).toBe(false);
	expect(terminal.span(coalesced)).toEqual({ timestamp: 1_000_000, frameOffset: 3 * 960, frames: 960 });
});

test("gaps closer than the warmup still play, the pre-roll carrying over the last one", () => {
	const terminal = new Terminal();
	terminal.clear(0, 80_000);
	for (let i = 0; i < 4; i++) {
		terminal.update(frame(0n, i * 20_000));
		terminal.decoded(packet(i * 20_000), Duration.micros(20_000));
	}
	for (const start of [1_000_000, 2_000_000]) {
		terminal.update(gap);
		terminal.update(frame(1n, start));
		expect(terminal.preroll().map((p) => p.timestamp)).toEqual([
			start - 80_000,
			start - 60_000,
			start - 40_000,
			start - 20_000,
		]);
		terminal.decoded(packet(start), Duration.micros(20_000));
		expect(terminal.span(opus(start)).frames).toBe(960);
	}
});

test("a short history trims only the warmup it leaves uncovered", () => {
	const terminal = new Terminal();
	terminal.clear(0, 80_000);
	terminal.update(frame(0n, 0));
	terminal.decoded(packet(0), Duration.micros(20_000));
	terminal.update(frame(0n, 20_000));
	// A frame of unknown duration cannot be restamped, so the pre-roll starts after it.
	terminal.decoded(packet(20_000), undefined);
	terminal.update(frame(0n, 40_000));
	terminal.decoded(packet(40_000), Duration.micros(20_000));
	terminal.update(gap);
	terminal.update(frame(1n, 1_000_000));
	expect(terminal.preroll().map((p) => p.timestamp)).toEqual([980_000]);
	const span = terminal.span({ timestamp: 1_000_000, sampleRate: 48_000, numberOfFrames: 4_800 });
	expect(span).toEqual({ timestamp: 1_060_000, frameOffset: 2_880, frames: 1_920 });
});

test("a fresh encoder after a declared end takes no pre-roll from the stopped one", () => {
	const terminal = new Terminal();
	terminal.clear(0, 80_000);
	terminal.update(frame(0n, 0));
	terminal.decoded(packet(0), Duration.micros(20_000));
	terminal.update(end(0n, 20_000));
	expect(terminal.update(frame(1n, 1_000_000))).toBe(true);
	terminal.update(gap);
	terminal.update(frame(2n, 2_000_000));
	expect(terminal.preroll()).toEqual([]);
	expect(terminal.span(opus(2_000_000)).frames).toBe(0);
});
