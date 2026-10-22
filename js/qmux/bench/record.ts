/**
 * Sweep QMux record decoding over record size x frames per record. Each frame is a STREAM frame
 * whose payload fills its share of the record, an empty STREAM frame, or a MAX_DATA control frame.
 * Decoding must cost what the record holds, so time per record stays flat as frames multiply.
 */

import * as Frame from "../src/frame.ts";
import * as Stream from "../src/stream.ts";
import { VarInt } from "../src/varint.ts";

const runs = 200;
const id = new Stream.Id(VarInt.from(4n));

/** `count` frames that `make` builds, concatenated into one record body. */
function record(count: number, make: () => Frame.Any): Uint8Array {
	const parts = Array.from({ length: count }, () => Frame.encode(make(), "qmux-01"));
	const out = new Uint8Array(parts.reduce((sum, part) => sum + part.byteLength, 0));
	let offset = 0;
	for (const part of parts) {
		out.set(part, offset);
		offset += part.byteLength;
	}
	return out;
}

function time(body: Uint8Array, count: number): number {
	const start = performance.now();
	for (let run = 0; run < runs; run++) {
		const frames = Frame.decodeRecord(body);
		if (frames.length !== count) throw new Error(`decoded ${frames.length} of ${count} frames`);
	}
	return ((performance.now() - start) * 1000) / runs;
}

console.log("record_bytes,kind,frames,us_per_record");
for (const size of [1_024, 16_380]) {
	for (const count of [1, 16, 256]) {
		// Four bytes of STREAM header (type, id, two-byte length) per frame.
		const payload = new Uint8Array(Math.max(0, Math.floor(size / count) - 4));
		const body = record(count, () => ({ type: "stream", id, data: payload, fin: false }));
		console.log(`${body.byteLength},stream,${count},${time(body, count).toFixed(1)}`);
	}
	for (const [kind, make] of [
		["empty", (): Frame.Any => ({ type: "stream", id, data: new Uint8Array(0), fin: false })],
		["max_data", (): Frame.Any => ({ type: "max_data", max: 1n << 20n })],
	] as const) {
		const one = Frame.encode(make(), "qmux-01").byteLength;
		const count = Math.floor(size / one);
		console.log(`${size},${kind},${count},${time(record(count, make), count).toFixed(1)}`);
	}
}
