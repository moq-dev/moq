/** Sweep frame and chunk size for a lite group stream decoded into a group and drained by a reader. */
import { Producer } from "../src/group.ts";
import { readFrames } from "../src/lite/group.ts";
import { Reader } from "../src/stream.ts";
import * as Varint from "../src/varint.ts";

const frameSizes = [16, 100, 1000];
const chunkSizes = [1200, 16 * 1024, 64 * 1024];
const framesPerGroup = 50;
// A backlog of tiny frames in one large chunk: the worst case for how long batching holds the first.
const burst = { frameSize: 16, chunkSize: 64 * 1024, frames: 3000 };
const frames = 200_000; // Decode about this many frames per case so each row takes similar time.
const scale = 1_000_000; // Timestamped frames, as every lite-05+ track carries.
// A chunk carrying many frames must cost less per frame than one carrying a single frame, or
// decoding has gone back to paying a wakeup per frame. Loose, since the fixed per-chunk cost is
// what the batched case amortizes and the machine is noisy.
const maxBatchedRatio = 0.8;
let checksum = 0;

/** A group stream's frames: a zigzag timestamp delta, a size, and the payload, split into chunks. */
function encode(frameSize: number, chunkSize: number, count: number): Uint8Array[] {
	const parts: Uint8Array[] = [];
	for (let index = 0; index < count; index++) {
		parts.push(Varint.encode(2 * 33_333), Varint.encode(frameSize), new Uint8Array(frameSize).fill(index));
	}
	const bytes = new Uint8Array(parts.reduce((sum, part) => sum + part.byteLength, 0));
	let offset = 0;
	for (const part of parts) {
		bytes.set(part, offset);
		offset += part.byteLength;
	}

	const chunks: Uint8Array[] = [];
	for (let start = 0; start < bytes.byteLength; start += chunkSize) {
		chunks.push(bytes.subarray(start, Math.min(start + chunkSize, bytes.byteLength)));
	}
	return chunks;
}

/** Decode one group, returning nanoseconds until the reader has every frame and until it had the first. */
async function run(chunks: Uint8Array[], count: number): Promise<{ total: number; first: number }> {
	// Queue every chunk up front so only the decode and the handoff are timed, not the source.
	const stream = new ReadableStream<Uint8Array>({
		start(controller) {
			for (const chunk of chunks) controller.enqueue(chunk);
			controller.close();
		},
	});
	const producer = new Producer(0);
	const consumer = producer.consume();

	const start = performance.now();
	let first = 0;
	const drained = (async () => {
		for (let count = 0; ; count++) {
			const frame = await consumer.readFrame();
			if (!frame) return count;
			if (count === 0) first = performance.now() - start;
			checksum += frame.payload[0];
		}
	})();

	await readFrames(new Reader(stream), producer, scale);
	producer.close();
	const read = await drained;
	const total = performance.now() - start;
	if (read !== count) throw new Error(`read ${read} of ${count} frames`);
	return { total: total * 1e6, first: first * 1e6 };
}

/** Decode `total` frames in groups of `count`, returning ns per frame and the mean µs to the first frame. */
async function measure(chunks: Uint8Array[], count: number, total: number): Promise<{ ns: number; first: number }> {
	const groups = Math.max(1, Math.floor(total / count));

	// Warm up so the first row doesn't pay for the JIT.
	for (let index = 0; index < 50; index++) await run(chunks, count);

	let elapsed = 0;
	let first = 0;
	for (let index = 0; index < groups; index++) {
		const result = await run(chunks, count);
		elapsed += result.total;
		first += result.first;
	}
	return { ns: elapsed / (groups * count), first: first / groups / 1000 };
}

function row(frameSize: number, chunkSize: number, perChunk: number, result: { ns: number; first: number }) {
	console.log(`${frameSize},${chunkSize},${perChunk.toFixed(1)},${result.ns.toFixed(0)},${result.first.toFixed(2)}`);
}

console.log("frame_bytes,chunk_bytes,frames_per_chunk,ns_per_frame,first_frame_us");

for (const frameSize of frameSizes) {
	let single: number | undefined;
	for (const chunkSize of [frameSize + 8, ...chunkSizes]) {
		const chunks = encode(frameSize, chunkSize, framesPerGroup);
		const perChunk = framesPerGroup / chunks.length;
		const result = await measure(chunks, framesPerGroup, frames);
		row(frameSize, chunkSize, perChunk, result);

		if (single === undefined) {
			single = result.ns;
		} else if (perChunk >= 10 && result.ns > single * maxBatchedRatio) {
			throw new Error(
				`${frameSize} byte frames at ${perChunk.toFixed(1)} per chunk cost ${result.ns.toFixed(0)} ns/frame, over ${maxBatchedRatio}x the ${single.toFixed(0)} ns of one per chunk`,
			);
		}
	}
}

{
	const chunks = encode(burst.frameSize, burst.chunkSize, burst.frames);
	row(burst.frameSize, burst.chunkSize, burst.frames / chunks.length, await measure(chunks, burst.frames, frames));
}

if (checksum === 0) throw new Error("benchmark did no work");
