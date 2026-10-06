/** Decode Rust's lite varints and messages, then echo JS's own encoding of them back. */
import assert from "node:assert/strict";
import { Datagram } from "../../js/net/src/lite/datagram.ts";
import { frameDecoder, Group } from "../../js/net/src/lite/group.ts";
import { Setup } from "../../js/net/src/lite/setup.ts";
import { Version } from "../../js/net/src/lite/version.ts";
import { Reader, Writer } from "../../js/net/src/stream.ts";

const input: {
	version: string;
	values: number;
	varints: number[];
	setup: number[];
	datagram: number[];
	group: number[];
} = JSON.parse(process.argv[2]);

const versions: Record<string, Version> = {
	"moq-lite-06": Version.DRAFT_06,
	"moq-lite-07-wip": Version.DRAFT_07,
};
const version = versions[input.version];
assert(version !== undefined, `unknown version ${input.version}`);

const reader = (bytes: number[]) => new Reader(undefined, Uint8Array.from(bytes), version);

// Collects whatever `f` writes with a Writer on this version.
async function write(f: (w: Writer) => Promise<void>): Promise<number[]> {
	const out: number[] = [];
	const w = new Writer(new WritableStream<Uint8Array>({ write: (chunk) => void out.push(...chunk) }), version);
	await f(w);
	w.close();
	await w.closed;
	return out;
}

// Varints: decode every value Rust wrote, then write them back.
const r = reader(input.varints);
const values: bigint[] = [];
for (let i = 0; i < input.values; i++) values.push(await r.u62());
assert(await r.done(), "trailing varint bytes");
const varints = await write(async (w) => {
	for (const v of values) await w.u62(v);
});

// Past 2^62-1 the range is per version: lite-07 carries the full 64 bits, which JS writes and
// Rust reads back; lite-06's QUIC form cannot express it, so JS refuses to write it, as Rust does.
const huge = [1n << 62n, (1n << 64n) - 1n];
let beyond: number[] = [];
if (version === Version.DRAFT_07) {
	beyond = await write(async (w) => {
		for (const v of huge) await w.u62(v);
	});
	const back = reader(beyond);
	for (const v of huge) assert.equal(await back.u62(), v);
} else {
	for (const v of huge) await assert.rejects(write((w) => w.u62(v)));
}

const setup = await Setup.decode(reader(input.setup), version);
const setupBytes = await write((w) => setup.encode(w, version));

const datagram = await Datagram.decode(Uint8Array.from(input.datagram), version);
const datagramBytes = [...datagram.encode(version)];

// A GROUP header, then frames until the stream ends.
const g = reader(input.group);
const group = await Group.decode(g, version);
const decode = frameDecoder(1_000_000);
const frames: { timestamp: bigint; payload: Uint8Array }[] = [];
for (;;) {
	const frame = await g.decodeMaybe(decode);
	if (!frame) break;
	frames.push({ timestamp: BigInt(frame.timestamp.value), payload: frame.payload });
}
const zigzag = (d: bigint) => (d << 1n) ^ (d >> 63n);
const groupBytes = await write(async (w) => {
	await group.encode(w, version);
	let prev = 0n;
	for (const { timestamp, payload } of frames) {
		await w.u62(zigzag(timestamp - prev));
		prev = timestamp;
		await w.u53(payload.byteLength);
		await w.write(payload);
	}
});

console.log(
	JSON.stringify({
		values: values.map((v) => v.toString()),
		varints,
		setup: setupBytes,
		datagram: datagramBytes,
		group: groupBytes,
		beyond,
	}),
);
