import { expect, test } from "bun:test";
import { Time } from "@moq/net";
import type { InitSegment } from "./decode.ts";
import { Format } from "./format.ts";

const TIMESCALE = 90_000;
const INIT: InitSegment = {
	timescale: TIMESCALE,
	trackId: 1,
	kind: "video",
	defaultSampleDuration: 0,
	defaultSampleSize: 0,
	defaultSampleFlags: 0,
};

function box(type: string, ...parts: Uint8Array[]): Uint8Array {
	const size = 8 + parts.reduce((sum, part) => sum + part.byteLength, 0);
	const out = new Uint8Array(size);
	const view = new DataView(out.buffer);
	view.setUint32(0, size);
	out.set(new TextEncoder().encode(type), 4);
	let offset = 8;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.byteLength;
	}
	return out;
}

function u32(...values: number[]): Uint8Array {
	const out = new Uint8Array(values.length * 4);
	const view = new DataView(out.buffer);
	for (const [i, value] of values.entries()) view.setInt32(i * 4, value);
	return out;
}

type Run = [duration: number, cts: number][];

/** A moof+mdat at `tfdt` ticks whose runs list one-byte samples in decode order. */
function fragment(tfdt: number, ...runs: Run[]): Uint8Array {
	const count = runs.reduce((sum, run) => sum + run.length, 0);
	const moof = (dataOffset: number) => {
		let offset = dataOffset;
		const truns = runs.map((run) => {
			// Version 1 (signed CTS): data-offset, sample-duration, sample-size, and CTS present.
			const trun = box("trun", u32(0x01000b01, run.length, offset), ...run.map(([d, cts]) => u32(d, 1, cts)));
			offset += run.length;
			return trun;
		});
		return box(
			"moof",
			box("mfhd", u32(0, 0)),
			box(
				"traf",
				box("tfhd", u32(0x020000, 1)),
				box("tfdt", u32(0x01000000, Math.floor(tfdt / 2 ** 32), tfdt % 2 ** 32)),
				...truns,
			),
		);
	};
	const header = moof(0);
	return new Uint8Array([...moof(header.byteLength + 8), ...box("mdat", new Uint8Array(count))]);
}

// The frame timestamp is the broadcast timeline: a passthrough fragment whose `tfdt` still
// carries its source PTS decodes at the frame timestamp, keeping its B-frame order.
test("CmafFormat times samples from the frame timestamp", () => {
	const source = 3_600 * TIMESCALE;
	// I, P, B in decode order, presenting at +0, +2, and +1 frames.
	const segment = fragment(source, [
		[3000, 3000],
		[3000, 6000],
		[3000, 0],
	]);

	const frames = new Format(INIT).decode(segment, Time.Timestamp.fromMillis(10_000));
	expect(frames.map((f) => f.timestamp)).toEqual([10_000_000, 10_066_667, 10_033_333] as Time.Micro[]);
});

// A fragment can open on a sample that presents after a later one (open-GOP leading pictures,
// or a cut mid-GOP). The earliest presentation time is the anchor, not the first sample.
test("CmafFormat anchors the earliest presentation time", () => {
	const segment = fragment(0, [
		[3000, 6000],
		[3000, 0],
	]);

	const frames = new Format(INIT).decode(segment, Time.Timestamp.fromMillis(5_000));
	expect(frames.map((f) => f.timestamp)).toEqual([5_033_333, 5_000_000] as Time.Micro[]);
});

// Runs continue one decode timeline, so the anchor is the earliest sample across all of them.
test("CmafFormat anchors the earliest sample across runs", () => {
	const segment = fragment(0, [[3000, 6000]], [[3000, 0]]);

	const frames = new Format(INIT).decode(segment, at(3000));
	expect(frames.map((f) => f.timestamp)).toEqual([66_667, 33_333] as Time.Micro[]);
});

// An untimed track's frames carry no broadcast time, so their samples present at `tfdt`.
test("CmafFormat times an untimed fragment from its tfdt", () => {
	const segment = fragment(TIMESCALE, [
		[3000, 3000],
		[3000, 6000],
		[3000, 0],
	]);

	const frames = new Format(INIT).decode(segment, undefined);
	expect(frames.map((f) => f.timestamp)).toEqual([1_033_333, 1_100_000, 1_066_667] as Time.Micro[]);
});

// Samples are read front to back, so a run that skips bytes is refused rather than sliced wrong.
test("CmafFormat refuses a run that doesn't start at the next sample", () => {
	const segment = fragment(0, [[3000, 0]], [[3000, 0]]);
	const view = new DataView(segment.buffer);
	// The second trun's data_offset follows its type, version/flags, and sample count.
	const trun = segment.findLastIndex((_, i) => new TextDecoder().decode(segment.subarray(i, i + 4)) === "trun");
	view.setInt32(trun + 12, view.getInt32(trun + 12) + 1);

	expect(() => new Format(INIT).decode(segment, undefined)).toThrow(/data_offset/);
});

function at(ticks: number): Time.Timestamp {
	return new Time.Timestamp(ticks, Time.Timescale(TIMESCALE));
}
