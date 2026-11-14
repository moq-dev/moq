import { expect, test } from "bun:test";
import { ProtocolViolation, StreamCode, StreamError } from "../error.ts";
import { Producer } from "../group.ts";
import { hooks } from "../internal.ts";
import { concat, encodeVarint, Reader } from "../stream.ts";
import { SEGMENT_BYTES } from "../util/limits.ts";
import * as Varint from "../varint.ts";
import { frameDecoder, readFrames } from "./group.ts";
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
	const decoded = await streamOf([compressed], version).decode(
		frameDecoder({ scale: SCALE, compress: true, lengthFirst }),
	);
	expect(decoded.size).toBe(2);
	expect(decoded.inflated).toBe(9);
	expect(decoded.timestamp?.value).toBe(7);
	expect(decoded.payload).toEqual(new Uint8Array([5, 6]));
});

// A lite-07 Payload Length of 0 is reserved for GROUP_END, which no version defines yet. A malformed
// FRAME ends only its stream, so it is a stream error rather than a session's protocol violation.
test("a lite-07 FRAME with a Payload Length of 0 is malformed", async () => {
	const producer = new Producer(0n);
	const bytes = concat([encodeVarint(0, Version.DRAFT_07), encodeVarint(0, Version.DRAFT_07)]);
	const err = await readFrames(streamOf([bytes], Version.DRAFT_07), producer, {
		scale: SCALE,
		lengthFirst: true,
	}).then(
		() => undefined,
		(err: unknown) => err,
	);
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.MalformedTrack);
});

// One macrotask, so every reaction queued before it has run.
function nextTask(): Promise<void> {
	return new Promise((resolve) => {
		const channel = new MessageChannel();
		channel.port1.onmessage = () => {
			channel.port1.close();
			channel.port2.close();
			resolve();
		};
		channel.port2.postMessage(null);
	});
}

// Whether `work` settles within a few event-loop turns.
async function settles(work: Promise<unknown>): Promise<boolean> {
	let settled = false;
	const done = () => {
		settled = true;
	};
	work.then(done, done);
	for (let i = 0; i < 10 && !settled; i++) await nextTask();
	return settled;
}

// A lite-07 continuation re-sending a held frame past one segment skips its payload. Once the group
// closes, the skip stops waiting on a peer that stopped sending, for a frame held whole and for the
// one a YIELDED reset cut short alike.
test.each([false, true])("a continuation's held-frame skip stops once the group closes (cut: %p)", async (cut) => {
	const version = Version.DRAFT_07;
	const size = SEGMENT_BYTES + 10;
	const producer = new Producer(0n);
	let partial: ReturnType<Producer["createFrame"]> | undefined;
	if (cut) {
		partial = producer.createFrame({ size });
		partial.write(new Uint8Array(5));
	} else producer.writeFrame({ payload: new Uint8Array([1]) });

	// The frame again, with fewer bytes than the skip needs, then nothing: the stream never ends.
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	const stream = new Reader(
		new ReadableStream<Uint8Array>({
			start: (c) => {
				controller = c;
			},
		}),
		undefined,
		version,
	);
	controller.enqueue(concat([frameHeader(version, undefined, undefined, size, true), new Uint8Array(2)]));
	const done = readFrames(stream, producer, { scale: 0, lengthFirst: true }, { index: 0, partial });
	expect(await settles(done)).toBe(false);

	producer.abort(new StreamError(StreamCode.Cancel));
	expect(await settles(done)).toBe(true);
	await done;
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

// A peer's huge Frame Start numbers a frame past what a number counts exactly, which is refused
// as a protocol violation rather than counted wrong. Mirrors the Rust
// `group_frame_start_past_any_count_is_refused`.
test("a lite-07 frame numbered past the safe integer range is refused", async () => {
	const version = Version.DRAFT_07;
	const frame = concat([frameHeader(version, 0n, undefined, 1, true), new Uint8Array([7])]);
	const start = Number.MAX_SAFE_INTEGER - 1;
	const producer = new Producer(0n);
	hooks.startGroup(producer, start);
	const err = await readFrames(
		streamOf([frame, frame], version),
		producer,
		{ scale: SCALE, lengthFirst: true },
		{
			index: start,
		},
	).then(
		() => undefined,
		(err: unknown) => err,
	);
	expect(err).toBeInstanceOf(ProtocolViolation);
	expect(producer.consume().frameCount).toBe(Number.MAX_SAFE_INTEGER);
});
