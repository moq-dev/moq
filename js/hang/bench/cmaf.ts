/**
 * Sweep CMAF fragment and LOCMAF object decode over mdat bytes x declared sample count. A valid run
 * reads every declared sample; a hostile run declares 2^32 - 1 defaulted samples over the same
 * bytes, and must cost what its bytes do, not what its count claims. A LOCMAF row times two
 * objects: a full header and the empty delta, the steady state, that refers to it.
 */

import { Varint } from "@moq/net";
import { decodeDataSegment, type InitSegment } from "../src/cmaf/decode.ts";
import { Format as Locmaf } from "../src/locmaf/format.ts";

const runs = 200;

/** A box of `type` around `parts`. */
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

function words(...values: number[]): Uint8Array {
	const out = new Uint8Array(4 * values.length);
	for (const [i, value] of values.entries()) new DataView(out.buffer).setUint32(4 * i, value);
	return out;
}

/** One run of `count` samples sized by the tfhd default, over `bytes` of mdat. */
function fragment(count: number, size: number, bytes: number): Uint8Array {
	// tfhd: default-base-is-moof and default_sample_size.
	const tfhd = box("tfhd", words(0x02_0010, 1, size));
	const moof = box("moof", box("mfhd", words(0, 1)), box("traf", tfhd, box("trun", words(0, count))));
	return new Uint8Array([...moof, ...box("mdat", new Uint8Array(bytes))]);
}

const init: InitSegment = {
	timescale: 90_000,
	trackId: 1,
	kind: "video",
	defaultSampleDuration: 3_000,
	defaultSampleSize: 0,
	defaultSampleFlags: 0,
};

/** A LOCMAF full header (element 2) for `count` samples of the default `size`, then the mdat payload. */
function locmaf(count: number, size: number, bytes: number): { full: Uint8Array; delta: Uint8Array } {
	const vi64 = (value: number) => [...Varint.encodeLeadingOnes(value)];
	// tfhdDefaultSampleSize (6), tfdtBaseMediaDecodeTime (10), trunSampleCount (14).
	const block = [...vi64(6), ...vi64(size), ...vi64(10), ...vi64(0), ...vi64(14), ...vi64(count)];
	const mdat = new Uint8Array(bytes);
	return {
		full: new Uint8Array([2, ...vi64(block.length), ...block, ...mdat]),
		// An empty delta (element 3): every field as before.
		delta: new Uint8Array([3, 0, ...mdat]),
	};
}

function time(decode: () => number, hostile: boolean): { samples: number; micros: number } {
	let samples = 0;
	const start = performance.now();
	for (let run = 0; run < runs; run++) {
		try {
			samples = decode();
			if (hostile) throw new Error("a hostile count decoded");
		} catch (err) {
			if (!hostile || !String(err).includes("mdat bytes")) throw err;
		}
	}
	return { samples, micros: ((performance.now() - start) * 1000) / runs };
}

const cmaf = (segment: Uint8Array) => () => decodeDataSegment(segment, init, undefined).length;
const delta = (objects: { full: Uint8Array; delta: Uint8Array }) => {
	const format = new Locmaf(init);
	return () => {
		format.decode(objects.full, undefined);
		return format.decode(objects.delta, undefined).length;
	};
};

console.log("container,mdat_bytes,declared,decoded,us_per_fragment");
for (const [name, build] of [
	["cmaf", (count: number, size: number, bytes: number) => cmaf(fragment(count, size, bytes))],
	["locmaf", (count: number, size: number, bytes: number) => delta(locmaf(count, size, bytes))],
] as const) {
	for (const bytes of [1_024, 65_536]) {
		for (const count of [1, 64, 1_024]) {
			const { samples, micros } = time(build(count, bytes / count, bytes), false);
			if (samples !== count) throw new Error(`${name} decoded ${samples} of ${count} samples`);
			console.log(`${name},${bytes},${count},${samples},${micros.toFixed(1)}`);
		}
		const hostile = time(build(0xffff_ffff, 1, bytes), true);
		console.log(`${name},${bytes},${0xffff_ffff},0,${hostile.micros.toFixed(1)}`);
	}
}
