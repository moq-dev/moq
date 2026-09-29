/** Decode Rust's varint encodings with js/net's VarInt, then hand back js/net's own encodings. */
import assert from "node:assert/strict";
import { Version } from "../../js/net/src/ietf/version.ts";
import { Reader, Writer } from "../../js/net/src/stream.ts";
import { VarInt } from "../../js/net/src/varint.ts";

// Each value is a decimal string, since JSON numbers round past 2^53.
const input: { values: string[]; quic: number[][]; leadingOnes: number[][] } = JSON.parse(process.argv[2]);

async function encode(v: VarInt, version?: Version): Promise<number[]> {
	const bytes: number[] = [];
	const writer = new Writer(new WritableStream<Uint8Array>({ write: (chunk) => void bytes.push(...chunk) }), version);
	await writer.varint(v);
	writer.close();
	await writer.closed;
	return bytes;
}

async function decode(bytes: number[], version?: Version): Promise<VarInt> {
	const reader = new Reader(undefined, new Uint8Array(bytes), version);
	const v = await reader.varint();
	assert(await reader.done(), `trailing bytes after ${v}`);
	return v;
}

const output: { quic: number[][]; leadingOnes: number[][] } = { quic: [], leadingOnes: [] };
for (const [i, value] of input.values.entries()) {
	const expected = VarInt.fromBigInt(BigInt(value));
	for (const [format, version] of [
		["quic", undefined],
		["leadingOnes", Version.DRAFT_17],
	] as const) {
		const decoded = await decode(input[format][i], version);
		assert(decoded.equals(expected), `${format}: decoded ${decoded}, expected ${value}`);
		assert.equal(decoded.toString(), value);
		if (BigInt(value) <= BigInt(Number.MAX_SAFE_INTEGER)) assert.equal(decoded.toNumber(), Number(value));
		else assert.throws(() => decoded.toNumber(), RangeError);
		output[format].push(await encode(expected, version));
	}
}

// Stdout is the encoding channel back to Rust.
console.log(JSON.stringify(output));
