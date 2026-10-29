import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import type { Micro, Milli } from "@moq/clock";
import { Track, Varint } from "@moq/net";
import type { InitSegment } from "../cmaf/decode.ts";
import { Format as CmafFormat } from "../cmaf/format.ts";
import { Consumer } from "../container/consumer.ts";
import type { Frame } from "../container/types.ts";
import { Format } from "./format.ts";

// Objects are built field by field from draft-einarsson-moq-locmaf-01 Sections 7 to 12.

/** ISO sample flags: a sync sample (`sample_depends_on` 2) and a non-sync one (`sample_depends_on` 1). */
const SYNC = 0x0200_0000;
const NON_SYNC = 0x0101_0000;

const VIDEO: InitSegment = {
	timescale: 90_000,
	trackId: 1,
	kind: "video",
	defaultSampleDuration: 3000,
	defaultSampleSize: 0,
	defaultSampleFlags: NON_SYNC,
};

const vi64 = (value: number) => [...Varint.encodeLeadingOnes(value)];
const zigzag = (value: number) => (value >= 0 ? 2 * value : -2 * value - 1);

/** A header field: an even ID's vi64, or an odd ID's list of vi64 elements, each already encoded for its context. */
type Field = [id: number, value: number | number[]];

/** A full (2) or delta (3) header element. */
function header(type: 2 | 3, fields: Field[]): number[] {
	const block: number[] = [];
	for (const [id, value] of fields) {
		block.push(...vi64(id));
		if (typeof value === "number") {
			block.push(...vi64(value));
		} else {
			const bytes = value.flatMap(vi64);
			block.push(...vi64(bytes.length), ...bytes);
		}
	}
	return [...vi64(type), ...vi64(block.length), ...block];
}

/** A genBox element carrying a `type` box with `payload`. */
function genBox(type: string, payload: number[]): number[] {
	return [1, ...vi64(4 + payload.length), ...new TextEncoder().encode(type), ...payload];
}

/** An mdat payload whose sample `i` is `sizes[i]` bytes of `i`. */
function mdat(...sizes: number[]): number[] {
	return sizes.flatMap((size, i) => new Array(size).fill(i));
}

function object(...parts: number[][]): Uint8Array {
	return new Uint8Array(parts.flat());
}

const summary = (frames: Frame[]) =>
	frames.map((f) => ({
		size: f.payload.byteLength,
		first: f.payload[0],
		timestamp: f.timestamp,
		duration: f.duration,
		keyframe: f.keyframe,
	}));

test("decodes a full header and the delta headers after it in one group", () => {
	const format = new Format(VIDEO);

	// Section 11: three samples at BMDT 0, the first a sync sample. The last size comes from the mdat.
	const full = object(
		header(2, [
			[1, [400, 410]], // trunSampleSizes, n - 1 entries
			[10, 0], // tfdtBaseMediaDecodeTime
			[12, SYNC], // trunFirstSampleFlags
			[14, 3], // trunSampleCount
		]),
		mdat(400, 410, 420),
	);
	expect(summary(format.decode(full, undefined))).toEqual([
		{ size: 400, first: 0, timestamp: 0 as Micro, duration: 33_333 as Micro, keyframe: true },
		{ size: 410, first: 1, timestamp: 33_333 as Micro, duration: 33_333 as Micro, keyframe: false },
		{ size: 420, first: 2, timestamp: 66_667 as Micro, duration: 33_333 as Micro, keyframe: false },
	]);

	// Section 12: a prft genBox ahead of a delta that deletes the first-sample flags and grows
	// each listed size by 100. Its BMDT continues where the full chunk's samples ended.
	const next = object(
		genBox("prft", new Array(20).fill(0)),
		header(3, [
			[1, [zigzag(100), zigzag(100)]],
			[27, [12]], // deltaDeletedLocmafIDs
		]),
		mdat(500, 510, 520),
	);
	expect(summary(format.decode(next, undefined))).toEqual([
		{ size: 500, first: 0, timestamp: 100_000 as Micro, duration: 33_333 as Micro, keyframe: false },
		{ size: 510, first: 1, timestamp: 133_333 as Micro, duration: 33_333 as Micro, keyframe: false },
		{ size: 520, first: 2, timestamp: 166_667 as Micro, duration: 33_333 as Micro, keyframe: false },
	]);

	// Section 12.4: an empty delta changes nothing but the last size, which the mdat sets.
	const empty = object(header(3, []), mdat(500, 510, 600));
	expect(summary(format.decode(empty, undefined)).map((f) => [f.size, f.timestamp])).toEqual([
		[500, 200_000],
		[510, 233_333],
		[600, 266_667],
	]);
});

test("decodes signed composition offsets and a delta that grows the sample count", () => {
	const format = new Format(VIDEO);

	// I, P, B in decode order: offsets 0, +2 and -1 frames, zigzag even in a full header.
	const full = object(
		header(2, [
			[1, [5, 6]],
			[5, [zigzag(0), zigzag(6000), zigzag(-3000)]],
			[10, 90_000],
			[12, SYNC],
			[14, 3],
		]),
		mdat(5, 6, 7),
	);
	expect(format.decode(full, undefined).map((f) => f.timestamp)).toEqual([
		1_000_000, 1_100_000, 1_033_333,
	] as Micro[]);

	// Section 12.1.1: two more samples. Listed elements past the previous list's end are absolute,
	// and the offsets list the header leaves out keeps its three and gains zeros.
	const grown = object(
		header(3, [
			[1, [zigzag(0), zigzag(0), zigzag(8), zigzag(9)]],
			[14, zigzag(2)],
			[27, [12]],
		]),
		mdat(5, 6, 8, 9, 10),
	);
	expect(summary(format.decode(grown, undefined)).map((f) => [f.size, f.timestamp])).toEqual([
		[5, 1_100_000],
		[6, 1_200_000],
		[8, 1_133_333],
		[9, 1_200_000],
		[10, 1_233_333],
	]);
});

test("refuses a delta header with no full header before it in its group", () => {
	expect(() => new Format(VIDEO).decode(object(header(3, []), mdat(10)), undefined)).toThrow("without a full header");
});

test("refuses CENC fields", () => {
	// sencPerSampleIVSize, Section 10.3.
	const encrypted = object(
		header(2, [
			[10, 0],
			[14, 1],
			[16, 8],
		]),
		mdat(10),
	);
	expect(() => new Format(VIDEO).decode(encrypted, undefined)).toThrow("CENC");
});

test("refuses a sample count its mdat cannot hold", () => {
	const hostile = object(
		header(2, [
			[10, 0],
			[14, 2 ** 40],
		]),
		mdat(10),
	);
	expect(() => new Format(VIDEO).decode(hostile, undefined)).toThrow("mdat bytes");
});

const FIXTURES = JSON.parse(readFileSync(new URL("../cmaf/fixtures.json", import.meta.url), "utf8"));
const fragment = (name: string) => new Uint8Array(Buffer.from(FIXTURES[name], "base64"));

test("decodes rawBoxes as a plain CMAF chunk, which restarts the delta chain", () => {
	const init: InitSegment = { ...VIDEO, defaultSampleDuration: 0, defaultSampleFlags: 0 };
	const chunk = fragment("untimedKey");
	const format = new Format(init);

	// Section 9: element type 4, then the moof+mdat verbatim.
	const frames = format.decode(object([4], [...chunk]), undefined);
	expect(frames).toEqual(new CmafFormat(init).decode(chunk, undefined));
	expect(frames.length).toBeGreaterThan(0);

	// Section 9.3: a delta after rawBoxes has no reference until the next full header.
	format.decode(
		object(
			header(2, [
				[10, 0],
				[14, 1],
			]),
			mdat(10),
		),
		undefined,
	);
	format.decode(object([4], [...chunk]), undefined);
	expect(() => format.decode(object(header(3, []), mdat(10)), undefined)).toThrow("without a full header");
});

test("Consumer decodes each LOCMAF group against its own delta chain", async () => {
	const track = new Track.Producer("test");
	const consumer = new Consumer(track.subscribe({ maxDelay: 30_000 as Milli }), {
		format: new Format(VIDEO),
		maxDelay: 5000 as Milli,
	});

	// One sample per chunk: group A starts at BMDT 0 and group B at 6000 ticks.
	const full = (bmdt: number, size: number) =>
		object(
			header(2, [
				[8, SYNC], // tfhdDefaultSampleFlags
				[10, bmdt],
				[14, 1],
			]),
			mdat(size),
		);
	const next = (size: number) => object(header(3, [[27, [8]]]), mdat(size));

	const a = track.createGroup(0);
	const b = track.createGroup(1);
	a.writeFrame({ payload: full(0, 10) });
	b.writeFrame({ payload: full(6000, 30) });

	// Both full headers decode before either delta arrives, as with two groups in flight at once.
	const first = await consumer.next();
	expect(first?.kind === "frame" && first.frame.timestamp).toBe(0 as Micro);

	a.writeFrame({ payload: next(20) });
	b.writeFrame({ payload: next(40) });
	a.finish();
	b.finish();
	track.finish();
	track.close();

	const rest: [number, number][] = [];
	for (;;) {
		const read = await consumer.next();
		if (!read) break;
		if (read.kind === "frame") rest.push([read.frame.payload.byteLength, read.frame.timestamp]);
	}
	// A's delta continues A at 3000 ticks, not B at 9000.
	expect(rest).toEqual([
		[20, 33_333],
		[30, 66_667],
		[40, 100_000],
	]);
	consumer.close();
});
