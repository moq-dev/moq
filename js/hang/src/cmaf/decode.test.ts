import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { decodeDataSegment, decodeInitSegment, type InitSegment } from "./decode.ts";

// Shared with moq-mux, which decodes the same fragments, so both readers resolve a sample's size,
// duration, and keyframe bit the same way.
const FIXTURES = join(
	dirname(fileURLToPath(import.meta.url)),
	"../../../../rs/moq-mux/src/container/fmp4/test_data/sample-defaults.json",
);

interface Fixture {
	name: string;
	init: string;
	fragment: string;
	samples: { timestamp: number; duration: number; keyframe: boolean; data: number[] }[];
}

const { cases }: { cases: Fixture[] } = JSON.parse(readFileSync(FIXTURES, "utf8"));

describe("CMAF sample defaults", () => {
	for (const fixture of cases) {
		test(fixture.name, () => {
			const init = decodeInitSegment(new Uint8Array(Buffer.from(fixture.init, "base64")));
			const samples = decodeDataSegment(new Uint8Array(Buffer.from(fixture.fragment, "base64")), init, undefined);
			expect(
				samples.map(({ timestamp, duration, keyframe, data }) => ({
					timestamp,
					duration,
					keyframe,
					data: Array.from(data),
				})),
			).toEqual(fixture.samples);
		});
	}
});

/** A box of `type` around `parts`, with a 32-bit size. */
function box(type: string, ...parts: Uint8Array[]): Uint8Array {
	const size = 8 + parts.reduce((sum, part) => sum + part.byteLength, 0);
	const out = new Uint8Array(size);
	new DataView(out.buffer).setUint32(0, size);
	out.set(new TextEncoder().encode(type), 4);
	let offset = 8;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.byteLength;
	}
	return out;
}

/** Big-endian 32-bit words. */
function words(...values: number[]): Uint8Array {
	const out = new Uint8Array(4 * values.length);
	for (const [i, value] of values.entries()) new DataView(out.buffer).setUint32(4 * i, value);
	return out;
}

const INIT: InitSegment = {
	timescale: 90_000,
	trackId: 1,
	kind: "video",
	defaultSampleDuration: 3_000,
	defaultSampleSize: 1,
	defaultSampleFlags: 0,
};

/** One traf holding `trun`, then an mdat of `data` bytes. */
function fragment(trun: Uint8Array, data: number): Uint8Array {
	const moof = box("moof", box("mfhd", words(0, 1)), box("traf", box("tfhd", words(0x02_0000, 1)), trun));
	const out = new Uint8Array(moof.byteLength + 8 + data);
	out.set(moof);
	out.set(box("mdat", new Uint8Array(data)), moof.byteLength);
	return out;
}

describe("CMAF untrusted counts and sizes", () => {
	test("a zero timescale is refused at initialization", () => {
		const init = new Uint8Array(Buffer.from(cases[0].init, "base64"));
		const type = Buffer.from(init).indexOf("mdhd");
		const payload = type + 4;
		const timescale = payload + 4 + (init[payload] === 1 ? 16 : 8);
		new DataView(init.buffer, init.byteOffset).setUint32(timescale, 0);
		expect(() => decodeInitSegment(init)).toThrow("mdhd timescale is zero");
	});

	test("a run without a sample table reads defaults until the mdat ends", () => {
		const valid = decodeDataSegment(fragment(box("trun", words(0, 3)), 3), INIT, undefined);
		expect(valid.map((sample) => sample.duration)).toEqual([33_333, 33_333, 33_333]);

		// The largest count a u32 holds, over three bytes of media: refused before any sample.
		expect(() => decodeDataSegment(fragment(box("trun", words(0, 0xffff_ffff)), 3), INIT, undefined)).toThrow(
			"trun declares 4294967295 samples of 1 bytes, past the 3 mdat bytes left",
		);
	});

	test("a sample table shorter than its count is refused before any sample", () => {
		// sample_size present: four bytes per entry, two entries for a declared million.
		const trun = box("trun", words(0x200, 1_000_000, 1, 1));
		expect(() => decodeDataSegment(fragment(trun, 2), INIT, undefined)).toThrow(
			"trun declares 1000000 samples but its table holds 2",
		);
	});

	test("a box size that cannot advance or that overruns its parent is refused", () => {
		// size 1 with a 64-bit size of zero: no progress.
		const stalled = new Uint8Array([0, 0, 0, 1, ...new TextEncoder().encode("free"), 0, 0, 0, 0, 0, 0, 0, 0]);
		expect(() => decodeDataSegment(stalled, INIT, undefined)).toThrow("Invalid free box size 0");
		// Smaller than its own header.
		expect(() => decodeDataSegment(words(4, 0x6672_6565), INIT, undefined)).toThrow("Invalid free box size 4");
		// Larger than the bytes left.
		expect(() => decodeDataSegment(words(64, 0x6672_6565), INIT, undefined)).toThrow("Invalid free box size 64");
		// A child overrunning its parent, though the input continues past it.
		const overrun = box("moof", words(16, 0x7472_6166));
		const mdat = box("mdat", new Uint8Array(32));
		expect(() => decodeDataSegment(new Uint8Array([...overrun, ...mdat]), INIT, undefined)).toThrow(
			"Invalid traf box size 16",
		);
		expect(() => decodeInitSegment(stalled)).toThrow("Invalid free box size 0");
	});
});

test("decodeInitSegment refuses an encrypted sample entry", () => {
	const fixtures = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "fixtures.json"), "utf8"));
	const init = new Uint8Array(Buffer.from(fixtures.videoInit, "base64"));
	expect(decodeInitSegment(init).kind).toBe("video");

	// The same init with its avc1 sample entry renamed encv, as a CENC-protected track's would be.
	const at = Buffer.from(init).indexOf("avc1");
	init.set(new TextEncoder().encode("encv"), at);
	expect(() => decodeInitSegment(init)).toThrow("Encrypted (CENC) sample entry encv is unsupported");
});
