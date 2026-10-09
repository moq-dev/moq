import { expect, test } from "bun:test";
import { Producer } from "../group.ts";
import { Reader } from "../stream.ts";
import * as Varint from "../varint.ts";
import { readFrames } from "./group.ts";
import { Version } from "./version.ts";

const SCALE = 1000;

// The frames below are QUIC varints, so the streams read them as lite-06.
const VERSION = Version.DRAFT_06;

/** Frames as a group stream carries them: a zigzag timestamp delta when timestamped, then the sized payload. */
function encode(frames: { delta?: number; payload: number[] }[]): Uint8Array {
	const bytes: number[] = [];
	for (const { delta, payload } of frames) {
		if (delta !== undefined) bytes.push(...Varint.encode(delta < 0 ? -2 * delta - 1 : 2 * delta));
		bytes.push(...Varint.encode(payload.length), ...payload);
	}
	return new Uint8Array(bytes);
}

function streamOf(chunks: Uint8Array[]): Reader {
	return new Reader(
		new ReadableStream<Uint8Array>({
			start(controller) {
				for (const chunk of chunks) controller.enqueue(chunk);
				controller.close();
			},
		}),
		undefined,
		VERSION,
	);
}

test("every frame in a chunk reaches the reader before it wakes", async () => {
	const producer = new Producer(0);
	const consumer = producer.consume();
	const frames = Array.from({ length: 10 }, (_, index) => ({ delta: 1, payload: [index] }));
	const done = readFrames(streamOf([encode(frames)]), producer, SCALE);

	expect((await consumer.readFrame())?.payload).toEqual(new Uint8Array([0]));
	expect(consumer.frameCount).toBe(10);
	await done;
});

test("frames split at every byte keep their payloads and timestamps", async () => {
	const producer = new Producer(0);
	const consumer = producer.consume();
	// Deltas wide enough for multi-byte varints, and one that steps back.
	const frames = [
		{ delta: 100, payload: [1] },
		{ delta: 20_000, payload: [2, 2] },
		{ delta: -50, payload: [] },
		{ delta: 0, payload: Array.from({ length: 300 }, () => 4) },
	];
	const bytes = encode(frames);
	const chunks = Array.from(bytes, (byte) => new Uint8Array([byte]));

	await readFrames(streamOf(chunks), producer, SCALE);
	producer.close();

	let ts = 0;
	for (const { delta, payload } of frames) {
		const frame = await consumer.readFrame();
		ts += delta;
		expect(frame?.payload).toEqual(new Uint8Array(payload));
		expect(frame?.timestamp?.value).toBe(ts);
	}
	expect(await consumer.readFrame()).toBeUndefined();
});

test("frames without a timescale carry no timestamp prefix", async () => {
	const producer = new Producer(0);
	const consumer = producer.consume();
	await readFrames(streamOf([encode([{ payload: [7] }, { payload: [8, 9] }])]), producer, 0);
	producer.close();

	expect((await consumer.readFrame())?.payload).toEqual(new Uint8Array([7]));
	expect((await consumer.readFrame())?.payload).toEqual(new Uint8Array([8, 9]));
	expect(await consumer.readFrame()).toBeUndefined();
});

test("a stream that ends inside a frame rejects", async () => {
	const producer = new Producer(0);
	const bytes = encode([{ delta: 1, payload: [1, 2, 3] }]);
	await expect(readFrames(streamOf([bytes.subarray(0, bytes.byteLength - 1)]), producer, SCALE)).rejects.toThrow(
		"unexpected end of stream",
	);
});

test("stops once the group closes", async () => {
	const producer = new Producer(0);
	// Never ends: only the close can stop the read.
	const stream = new Reader(new ReadableStream<Uint8Array>(), undefined, VERSION);
	const done = readFrames(stream, producer, SCALE);
	producer.close();
	await done;
});
