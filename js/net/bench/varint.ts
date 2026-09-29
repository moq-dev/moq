/** Time one varint encode and one decode, for both wire formats, at their 1, 2, 4, and 8-byte sizes. */
import { Version } from "../src/ietf/version.ts";
import { Cursor } from "../src/stream.ts";
import * as Varint from "../src/varint.ts";

// Decodes run through one Cursor over this many copies, so the Cursor itself isn't timed.
const run = 1024;
const runs = 2_000;
const reps = 9;
const scratch = new ArrayBuffer(9);
let checksum = 0;

// QUIC varints (lite, and moq-transport before draft-17), and leading-ones (draft-17+), each at the
// largest value of its 1, 2, 4, and 8-byte forms that a `number` holds.
const formats = [
	{
		name: "quic",
		version: undefined,
		encode: Varint.encodeTo,
		values: [2 ** 6 - 1, 2 ** 14 - 1, 2 ** 30 - 1, Number.MAX_SAFE_INTEGER],
	},
	{
		name: "leading-ones",
		version: Version.DRAFT_17,
		encode: Varint.encodeLeadingOnesTo,
		values: [2 ** 7 - 1, 2 ** 14 - 1, 2 ** 28 - 1, Number.MAX_SAFE_INTEGER],
	},
];

interface Case {
	name: string;
	ops: [string, () => void][];
}

const cases: Case[] = [];
for (const format of formats) {
	for (const value of format.values) {
		const one = format.encode(scratch, value).slice();
		const encoded = new Uint8Array(one.byteLength * run);
		for (let i = 0; i < run; i++) encoded.set(one, i * one.byteLength);

		cases.push({
			name: `${format.name},${one.byteLength}-byte`,
			ops: [
				[
					"encode",
					() => {
						for (let i = 0; i < run; i++) checksum += format.encode(scratch, value).byteLength;
					},
				],
				[
					"decode",
					() => {
						const cursor = new Cursor(encoded, format.version);
						for (let i = 0; i < run; i++) checksum += cursor.u53();
					},
				],
				// The same, as the VarInt the generated codec will use, which allocates.
				[
					"decode-varint",
					() => {
						const cursor = new Cursor(encoded, format.version);
						for (let i = 0; i < run; i++) checksum += cursor.varint().lo;
					},
				],
			],
		});
	}
}

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

// Run every case once first, so the JIT has seen all of them and the first row isn't timing warmup.
for (const { ops } of cases) for (const [, body] of ops) for (let i = 0; i < runs; i++) body();

console.log("format,value,op,ns_per_op");
for (const { name, ops } of cases) {
	for (const [op, body] of ops) console.log(`${name},${op},${time(body).toFixed(1)}`);
}
if (checksum === 0) throw new Error("benchmark did no work");
