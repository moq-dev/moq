/** Time one varint encode and one decode, for both wire formats, at each QUIC varint size. */
import { Version } from "../src/ietf/version.ts";
import { Cursor } from "../src/stream.ts";
import * as Varint from "../src/varint.ts";

// Decodes run through one Cursor over this many copies, so the Cursor itself isn't timed.
const run = 1024;
const runs = 2_000;
const reps = 9;
const scratch = new ArrayBuffer(9);
let checksum = 0;

// QUIC varints (lite, and moq-transport before draft-17), and leading-ones (draft-17+).
const formats = [
	{ name: "quic", version: undefined, encode: Varint.encodeTo },
	{ name: "leading-ones", version: Version.DRAFT_17, encode: Varint.encodeLeadingOnesTo },
];
// The largest value of each QUIC size. In leading-ones form they take 1, 2, 5, and 8 bytes.
const values = [
	{ name: "1-byte", value: 2 ** 6 - 1 },
	{ name: "2-byte", value: 2 ** 14 - 1 },
	{ name: "4-byte", value: 2 ** 30 - 1 },
	{ name: "8-byte", value: Number.MAX_SAFE_INTEGER },
];

// Nanoseconds per varint for the fastest of several reps, since a slower one measured the machine.
function time(body: () => void): number {
	let best = Number.POSITIVE_INFINITY;
	for (let rep = 0; rep < reps; rep++) {
		const start = performance.now();
		for (let i = 0; i < runs; i++) body();
		best = Math.min(best, ((performance.now() - start) * 1e6) / (runs * run));
	}
	return best;
}

console.log("format,value,op,ns_per_op");
for (const format of formats) {
	for (const { name, value } of values) {
		const one = format.encode(scratch, value).slice();
		const encoded = new Uint8Array(one.byteLength * run);
		for (let i = 0; i < run; i++) encoded.set(one, i * one.byteLength);

		const encode = time(() => {
			for (let i = 0; i < run; i++) checksum += format.encode(scratch, value).byteLength;
		});
		const decode = time(() => {
			const cursor = new Cursor(encoded, format.version);
			for (let i = 0; i < run; i++) checksum += cursor.u53();
		});
		// The same, as the VarInt the generated codec will use, which allocates.
		const decodeVarInt = time(() => {
			const cursor = new Cursor(encoded, format.version);
			for (let i = 0; i < run; i++) checksum += cursor.varint().lo;
		});

		console.log(`${format.name},${name},encode,${encode.toFixed(1)}`);
		console.log(`${format.name},${name},decode,${decode.toFixed(1)}`);
		console.log(`${format.name},${name},decode-varint,${decodeVarInt.toFixed(1)}`);
	}
}
if (checksum === 0) throw new Error("benchmark did no work");
