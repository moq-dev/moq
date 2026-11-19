import { expect, test } from "bun:test";
import { ProtocolViolation, StreamCode, StreamError, TimestampMismatch } from "../error.ts";
import { Producer } from "../group.ts";
import { hooks } from "../internal.ts";
import { concat, encodeVarint, Reader, Writer } from "../stream.ts";
import { Timestamp } from "../time.ts";
import * as Varint from "../varint.ts";
import { frameDecoder, Group, readFrames, type WireFrame } from "./group.ts";
import { frameHeader } from "./publisher.ts";
import { hasLengthFirst, Version } from "./version.ts";

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

function streamOf(chunks: Uint8Array[], version: Version = VERSION): Reader {
	return new Reader(
		new ReadableStream<Uint8Array>({
			start(controller) {
				for (const chunk of chunks) controller.enqueue(chunk);
				controller.close();
			},
		}),
		undefined,
		version,
	);
}

test("every frame in a chunk reaches the reader before it wakes", async () => {
	const producer = new Producer(0n);
	const consumer = producer.consume();
	const frames = Array.from({ length: 10 }, (_, index) => ({ delta: 1, payload: [index] }));
	const done = readFrames(streamOf([encode(frames)]), producer, { scale: SCALE });

	expect((await consumer.readFrame())?.payload).toEqual(new Uint8Array([0]));
	expect(consumer.frameCount).toBe(10);
	await done;
});

test("frames split at every byte keep their payloads and timestamps", async () => {
	const producer = new Producer(0n);
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

	await readFrames(streamOf(chunks), producer, { scale: SCALE });
	producer.finish();

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
	const producer = new Producer(0n);
	const consumer = producer.consume();
	await readFrames(streamOf([encode([{ payload: [7] }, { payload: [8, 9] }])]), producer, { scale: 0 });
	producer.finish();

	expect((await consumer.readFrame())?.payload).toEqual(new Uint8Array([7]));
	expect((await consumer.readFrame())?.payload).toEqual(new Uint8Array([8, 9]));
	expect(await consumer.readFrame()).toBeUndefined();
});

test("a stream that ends inside a frame rejects", async () => {
	const producer = new Producer(0n);
	const bytes = encode([{ delta: 1, payload: [1, 2, 3] }]);
	await expect(
		readFrames(streamOf([bytes.subarray(0, bytes.byteLength - 1)]), producer, { scale: SCALE }),
	).rejects.toThrow("unexpected end of stream");
});

test("stops once the group closes", async () => {
	const producer = new Producer(0n);
	// Never ends: only the close can stop the read.
	const stream = new Reader(new ReadableStream<Uint8Array>(), undefined, VERSION);
	const done = readFrames(stream, producer, { scale: SCALE });
	producer.finish();
	await done;
});

// Lite-07 FRAME leads with its Payload Length, the payload size plus one, then the timestamp delta
// and inflated size; an earlier one ends its header with the size. Each version decodes what the
// publisher encodes, timestamps and inflated sizes included.
test.each([Version.DRAFT_06, Version.DRAFT_07])("a FRAME round-trips on %s", async (version) => {
	const lengthFirst = hasLengthFirst(version);
	expect(lengthFirst).toBe(version === Version.DRAFT_07);
	const frames = [
		{ delta: 100n, payload: [1] },
		{ delta: 20_000n, payload: [2, 2] },
		{ delta: -50n, payload: [] },
	];
	const encoded = frames.map(({ delta, payload }) =>
		concat([frameHeader(version, delta, undefined, payload.length, lengthFirst), new Uint8Array(payload)]),
	);
	// The first field is the size plus one on lite-07, and the timestamp delta before it.
	const first = encoded[0];
	if (lengthFirst) expect(Array.from(first.subarray(0, 1))).toEqual(Array.from(encodeVarint(2, version)));
	else expect(Array.from(first.subarray(first.length - 2))).toEqual([1, 1]);

	const producer = new Producer(0n);
	const consumer = producer.consume();
	await readFrames(streamOf(encoded, version), producer, { scale: SCALE, lengthFirst });
	producer.finish();
	let ts = 0;
	for (const { delta, payload } of frames) {
		const frame = await consumer.readFrame();
		ts += Number(delta);
		expect(frame?.payload).toEqual(new Uint8Array(payload));
		expect(frame?.timestamp?.value).toBe(ts);
	}
	expect(await consumer.readFrame()).toBeUndefined();

	// A compressed track's inflated size follows the timestamp delta, and the size still leads.
	const compressed = concat([frameHeader(version, 7n, 9, 2, lengthFirst), new Uint8Array([5, 6])]);
	const decoded = (await streamOf([compressed], version).decode(
		frameDecoder({ scale: SCALE, compress: true, lengthFirst }),
	)) as WireFrame;
	expect(decoded.size).toBe(2);
	expect(decoded.inflated).toBe(9);
	expect(decoded.timestamp?.value).toBe(7);
	expect(decoded.payload).toEqual(new Uint8Array([5, 6]));
});

// Lite-07 varints, as a Group Stream after its header carries them.
function lite07(...values: (number | bigint)[]): Uint8Array {
	return concat(values.map((value) => encodeVarint(value, Version.DRAFT_07)));
}

// What reading `bytes` as group `start`'s lite-07 frames resolves with, or the error it throws.
async function readLite07(bytes: Uint8Array, start = 0n, scale = SCALE): Promise<bigint | undefined | unknown> {
	const producer = new Producer(start);
	return readFrames(streamOf([bytes], Version.DRAFT_07), producer, { scale, lengthFirst: true }).catch(
		(err: unknown) => err,
	);
}

// A lite-07 GROUP_END, a Payload Length of 0 then the End, says where a whole group ends; a stream
// ending without one only closes it.
test("a lite-07 GROUP_END says where the group ends", async () => {
	const frame = [2, 0, 7];
	expect(await readLite07(lite07(...frame, 0, 500))).toBe(500n);
	expect(await readLite07(lite07(...frame))).toBeUndefined();
});

// A publisher that stalls between GROUP_END and its FIN can't hold the read open: the group's own
// close, such as an abort or eviction, still ends it.
test("a lite-07 GROUP_END still waiting for its FIN stops once the group closes", async () => {
	const producer = new Producer(0n);
	const stream = new Reader(
		new ReadableStream<Uint8Array>({
			start(controller) {
				controller.enqueue(lite07(2, 0, 7, 0, 500));
			},
		}),
		undefined,
		Version.DRAFT_07,
	);
	const consumer = producer.consume();
	const done = readFrames(stream, producer, { scale: SCALE, lengthFirst: true });
	// The frame and GROUP_END arrive together, so once the frame is read the FIN is all that's left.
	expect((await consumer.readFrame())?.payload).toEqual(new Uint8Array([7]));
	producer.abort(new Error("evicted"));
	expect(await done).toBeUndefined();
});

// An End at or before the group's start, or anything after GROUP_END, is malformed. A malformed
// FRAME ends only its stream, so it is a stream error rather than a session's protocol violation.
test.each([
	["an End at the group's start", lite07(0, 5), 5n],
	["an End before the group's start", lite07(0, 4), 5n],
	["a byte after GROUP_END", lite07(0, 9, 0), 5n],
	["a frame after GROUP_END", lite07(0, 9, 2, 0, 7), 5n],
])("a lite-07 GROUP_END with %s is malformed", async (_, bytes, start) => {
	const err = await readLite07(bytes, start);
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.MalformedTrack);
});

// A lite-07 FRAME on a scale-less track carries its timestamp, its group's start as a bare number;
// any other value is a TIMESTAMP_MISMATCH. The frame keeps the group's key as its time.
test("a scale-less lite-07 FRAME carries its group's start", async () => {
	const producer = new Producer(9n);
	const consumer = producer.consume();
	await readFrames(streamOf([lite07(2, 18, 7, 2, 0, 8)], Version.DRAFT_07), producer, {
		scale: 0,
		lengthFirst: true,
	});
	for (const payload of [7, 8]) {
		const frame = await consumer.readFrame();
		expect(frame?.payload).toEqual(new Uint8Array([payload]));
		expect(frame?.timestamp).toEqual(Timestamp.unscaled(9n));
	}
	for (const bytes of [lite07(2, 20, 7), lite07(2, 18, 7, 2, 2, 8)])
		expect(await readLite07(bytes, 9n, 0)).toBeInstanceOf(TimestampMismatch);
});

// GROUP carries the previous group's start plus one on lite-07, 0 when unknown, and passes over the
// starts strictly between; earlier versions carry nothing. Only lite-06 carries a Frame Start.
test("a lite-07 GROUP carries its Previous Start plus one", async () => {
	const encode = (previous: bigint | undefined, version: Version) =>
		Writer.collect(version, (w) => new Group({ subscribe: 1n, key: 7n, previous }).encode(w, version));
	expect(Array.from(await encode(4n, Version.DRAFT_07))).toEqual([3, 1, 7, 5]);
	expect(Array.from(await encode(undefined, Version.DRAFT_07))).toEqual([3, 1, 7, 0]);
	expect(Array.from(await encode(4n, Version.DRAFT_06))).toEqual([3, 1, 7, 0]);
	const partial = (version: Version) =>
		Writer.collect(version, (w) => new Group({ subscribe: 1n, key: 7n, frameStart: 4 }).encode(w, version));
	expect(Array.from(await partial(Version.DRAFT_06))).toEqual([3, 1, 7, 4]);
	await expect(partial(Version.DRAFT_07)).rejects.toThrow();

	const decode = (bytes: number[], version: Version) =>
		Group.decode(streamOf([new Uint8Array(bytes)], version), version);
	const group = await decode([3, 1, 7, 5], Version.DRAFT_07);
	expect([group.previous, group.passedOver(), group.frameStart]).toEqual([4n, { start: 5n, end: 7n }, 0]);
	expect((await decode([3, 1, 7, 0], Version.DRAFT_07)).passedOver()).toEqual({ start: 7n, end: 7n });
	expect((await decode([3, 1, 7, 0], Version.DRAFT_06)).previous).toBeUndefined();
});

// Stream end at the Payload Length ends a lite-07 group; within the rest of the header, it cut a frame.
test("a lite-07 stream ending at the Payload Length ends the group", async () => {
	const version = Version.DRAFT_07;
	const whole = concat([frameHeader(version, 1n, undefined, 1, true), new Uint8Array([7])]);
	const producer = new Producer(0n);
	await readFrames(streamOf([whole], version), producer, { scale: SCALE, lengthFirst: true });
	expect(producer.consume().frameCount).toBe(1);

	const cut = frameHeader(version, 1n, undefined, 1, true).subarray(0, 1);
	await expect(
		readFrames(streamOf([cut], version), new Producer(0n), { scale: SCALE, lengthFirst: true }),
	).rejects.toThrow("unexpected end of stream");
});

// A huge frame start, such as a FETCH's, numbers a frame past what a number counts exactly, which is
// refused as a protocol violation rather than counted wrong.
test("a lite-07 frame numbered past the safe integer range is refused", async () => {
	const version = Version.DRAFT_07;
	const frame = concat([frameHeader(version, 0n, undefined, 1, true), new Uint8Array([7])]);
	const start = Number.MAX_SAFE_INTEGER - 1;
	const producer = new Producer(0n);
	hooks.startGroup(producer, start);
	const err = await readFrames(streamOf([frame, frame], version), producer, { scale: SCALE, lengthFirst: true }).then(
		() => undefined,
		(err: unknown) => err,
	);
	expect(err).toBeInstanceOf(ProtocolViolation);
	expect(producer.consume().frameCount).toBe(Number.MAX_SAFE_INTEGER);
});
