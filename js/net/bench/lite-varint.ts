/** Lite-07 leading-ones varints against lite-06 QUIC varints, for what a publisher writes per frame, group, and request. */
import { randomHop } from "../src/hop.ts";
import { Datagram } from "../src/lite/datagram.ts";
import { Group } from "../src/lite/group.ts";
import { ProbeLevel, Setup } from "../src/lite/setup.ts";
import { Subscribe } from "../src/lite/subscribe.ts";
import { Version } from "../src/lite/version.ts";
import * as Path from "../src/path.ts";
import { type Cursor, Reader, Writer } from "../src/stream.ts";

const versions = [Version.DRAFT_06, Version.DRAFT_07];
const minMs = 200; // Run each case at least this long.
let checksum = 0;

/** A lite object: how the publisher writes it and how the subscriber reads it back. */
interface Sample {
	name: string;
	encode(w: Writer, version: Version): Promise<void>;
	decode(r: Reader, version: Version): Promise<unknown>;
}

// A FRAME header: the zigzag timestamp delta, then the size.
const header = (c: Cursor) => {
	c.u62();
	return c.u53();
};

// FRAME headers with the payloads left out, as (timestamp delta in µs, size) pairs.
function frames(name: string, headers: [bigint, number][]): Sample {
	const zigzag = (d: bigint) => (d << 1n) ^ (d >> 63n);
	return {
		name,
		async encode(w) {
			for (const [delta, size] of headers) {
				await w.u62(zigzag(delta));
				await w.u53(size);
			}
		},
		async decode(r) {
			// The subscriber's read loop, one synchronous decode per buffered frame, minus the payload.
			let n = 0;
			while (r.tryDecode(header) !== undefined) n++;
			return n;
		},
	};
}

const video = frames(
	"Video",
	Array.from({ length: 60 }, (_, n): [bigint, number] => {
		if (n === 0) return [0n, 60_000];
		return [33_333n, n % 10 === 0 ? 17_000 : 8_000];
	}),
);
const audio = frames(
	"Audio",
	Array.from({ length: 50 }, (_, n): [bigint, number] => [n === 0 ? 0n : 20_000n, 160]),
);

const group: Sample = {
	name: "Group",
	encode: (w, version) => new Group({ subscribe: 3n, sequence: 1_234 }).encode(w, version),
	decode: (r, version) => Group.decode(r, version),
};

const subscribe: Sample = {
	name: "Subscribe",
	encode: (w, version) =>
		new Subscribe({
			id: 3n,
			broadcast: Path.from("room/alice"),
			track: "video",
			priority: 2,
			maxDelay: 10_000,
		}).encode(w, version),
	decode: (r, version) => Subscribe.decode(r, version),
};

const datagram: Sample = {
	name: "Datagram",
	encode: (w, version) => w.write(new Datagram(3n, 1_234, 1_234_567_890, new Uint8Array()).encode(version)),
	decode: (r, version) => r.readAll().then((data) => Datagram.decode(data, version)),
};

const hop = randomHop();
const setup: Sample = {
	name: "Setup",
	encode: (w, version) => new Setup({ probe: ProbeLevel.Report, hop }).encode(w, version),
	decode: (r, version) => Setup.decode(r, version),
};

/** Collect what one encode writes. */
async function wire(sample: Sample, version: Version): Promise<Uint8Array> {
	const chunks: Uint8Array[] = [];
	const w = new Writer(new WritableStream<Uint8Array>({ write: (c) => void chunks.push(c.slice()) }), version);
	await sample.encode(w, version);
	w.close();
	await w.closed;
	const out = new Uint8Array(chunks.reduce((n, c) => n + c.byteLength, 0));
	let offset = 0;
	for (const c of chunks) {
		out.set(c, offset);
		offset += c.byteLength;
	}
	return out;
}

/** Time `f` until `minMs` has passed, returning ns per call. */
async function time(f: () => Promise<unknown>): Promise<number> {
	for (let i = 0; i < 1_000; i++) await f(); // warm up
	let calls = 0;
	const start = performance.now();
	let elapsed = 0;
	while (elapsed < minMs) {
		for (let i = 0; i < 1_000; i++) await f();
		calls += 1_000;
		elapsed = performance.now() - start;
	}
	return (elapsed * 1e6) / calls;
}

// A Writer whose sink discards, so encode is timed without collecting bytes.
const sink = () =>
	new WritableStream<Uint8Array>({
		write: (c) => {
			checksum += c.byteLength;
		},
	});

console.log("sample,version,bytes,encode_ns,decode_ns");
for (const sample of [video, audio, group, subscribe, datagram, setup]) {
	for (const version of versions) {
		const bytes = await wire(sample, version);
		const encode = await time(async () => {
			const w = new Writer(sink(), version);
			await sample.encode(w, version);
		});
		const decode = await time(async () => {
			checksum += Number((await sample.decode(new Reader(undefined, bytes, version), version)) !== undefined);
		});
		console.log(
			`${sample.name},${version.toString(16)},${bytes.byteLength},${encode.toFixed(0)},${decode.toFixed(0)}`,
		);
	}
}
if (checksum === 0) throw new Error("benchmark did no work");
