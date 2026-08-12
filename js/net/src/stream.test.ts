import { expect, test } from "bun:test";
import { SessionCode, SessionError, StreamCode, StreamError } from "./error.ts";
import { Reader, Stream, Writer } from "./stream.ts";

// Helper to create a writable stream that captures written data
function createTestWritableStream(): { stream: WritableStream<Uint8Array>; written: Uint8Array[] } {
	const written: Uint8Array[] = [];
	const stream = new WritableStream<Uint8Array>({
		write(chunk) {
			written.push(new Uint8Array(chunk));
		},
	});
	return { stream, written };
}

// Helper to concatenate written chunks
function concatChunks(chunks: Uint8Array[]): Uint8Array {
	const totalLength = chunks.reduce((sum, chunk) => sum + chunk.byteLength, 0);
	const result = new Uint8Array(totalLength);
	let offset = 0;
	for (const chunk of chunks) {
		result.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return result;
}

test("Writer u8", async () => {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);

	await writer.u8(42);
	await writer.u8(255);

	writer.close();
	await writer.closed;

	expect(written.length).toBe(2);
	expect(written[0]).toEqual(new Uint8Array([42]));
	expect(written[1]).toEqual(new Uint8Array([255]));
});

test("Writer i32", async () => {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);

	await writer.i32(0);
	await writer.i32(-1);
	await writer.i32(1000);

	writer.close();
	await writer.closed;

	const result = concatChunks(written);
	expect(result.byteLength).toBe(12); // 3 * 4 bytes

	// Read back the values
	const view = new DataView(result.buffer, result.byteOffset, result.byteLength);
	expect(view.getInt32(0)).toBe(0);
	expect(view.getInt32(4)).toBe(-1);
	expect(view.getInt32(8)).toBe(1000);
});

test("Writer u53", async () => {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);

	await writer.u53(0);
	await writer.u53(63); // MAX_U6
	await writer.u53(64); // MIN for 2-byte varint
	await writer.u53(16383); // MAX_U14
	await writer.u53(16384); // MIN for 4-byte varint

	writer.close();
	await writer.closed;

	// Verify the varint encoding sizes
	expect(written[0].byteLength).toBe(1); // 0 fits in 1 byte
	expect(written[1].byteLength).toBe(1); // 63 fits in 1 byte
	expect(written[2].byteLength).toBe(2); // 64 needs 2 bytes
	expect(written[3].byteLength).toBe(2); // 16383 fits in 2 bytes
	expect(written[4].byteLength).toBe(4); // 16384 needs 4 bytes
});

test("Writer string", async () => {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);

	await writer.string("hello");
	await writer.string("🎉");

	writer.close();
	await writer.closed;

	const result = concatChunks(written);

	// Create a reader to parse the result
	const reader = new Reader(undefined, result);

	const str1 = await reader.string();
	const str2 = await reader.string();

	expect(str1).toBe("hello");
	expect(str2).toBe("🎉");
});

test("Reader u8", async () => {
	const data = new Uint8Array([42, 255, 0, 128]);
	const reader = new Reader(undefined, data);

	expect(await reader.u8()).toBe(42);
	expect(await reader.u8()).toBe(255);
	expect(await reader.u8()).toBe(0);
	expect(await reader.u8()).toBe(128);

	expect(await reader.done()).toBe(true);
});

test("Reader read with exact sizes", async () => {
	const data = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
	const reader = new Reader(undefined, data);

	const chunk1 = await reader.read(3);
	expect(chunk1).toEqual(new Uint8Array([1, 2, 3]));

	const chunk2 = await reader.read(2);
	expect(chunk2).toEqual(new Uint8Array([4, 5]));

	const chunk3 = await reader.read(3);
	expect(chunk3).toEqual(new Uint8Array([6, 7, 8]));

	expect(await reader.done()).toBe(true);
});

test("Reader read with zero size", async () => {
	const data = new Uint8Array([1, 2, 3]);
	const reader = new Reader(undefined, data);

	const chunk = await reader.read(0);
	expect(chunk).toEqual(new Uint8Array([]));

	// Original data should still be available
	const remaining = await reader.read(3);
	expect(remaining).toEqual(new Uint8Array([1, 2, 3]));
});

test("Reader readAll", async () => {
	const data = new Uint8Array([1, 2, 3, 4, 5]);
	const reader = new Reader(undefined, data);

	// Read some data first
	await reader.read(2);

	// readAll should get the remaining data
	const remaining = await reader.readAll();
	expect(remaining).toEqual(new Uint8Array([3, 4, 5]));

	expect(await reader.done()).toBe(true);
});

test("Reader u53 varint decoding", async () => {
	// Test various varint sizes
	const testValues = [0, 63, 64, 16383, 16384, 1073741823, 1073741824];

	const { stream, written } = createTestWritableStream();
	const testWriter = new Writer(stream);

	for (const value of testValues) {
		await testWriter.u53(value);
	}

	testWriter.close();
	await testWriter.closed;

	const data = concatChunks(written);
	const reader = new Reader(undefined, data);

	for (const expectedValue of testValues) {
		const actualValue = await reader.u53();
		expect(actualValue).toBe(expectedValue);
	}

	expect(await reader.done()).toBe(true);
});

test("Reader u62 varint decoding", async () => {
	const testValues = [0n, 63n, 64n, 16383n, 16384n, 1073741823n, 1073741824n, 9007199254740991n]; // MAX_U53

	const { stream, written } = createTestWritableStream();
	const testWriter = new Writer(stream);

	for (const value of testValues) {
		await testWriter.u62(value);
	}

	testWriter.close();
	await testWriter.closed;

	const data = concatChunks(written);
	const reader = new Reader(undefined, data);

	for (const expectedValue of testValues) {
		const actualValue = await reader.u62();
		expect(actualValue).toBe(expectedValue);
	}

	expect(await reader.done()).toBe(true);
});

test("Reader string decoding", async () => {
	const testStrings = ["hello", "🎉", "", "world with spaces", "multi\nline\nstring"];

	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);

	for (const str of testStrings) {
		await writer.string(str);
	}

	writer.close();
	await writer.closed;

	const data = concatChunks(written);
	const reader = new Reader(undefined, data);

	for (const expectedString of testStrings) {
		const actualString = await reader.string();
		expect(actualString).toBe(expectedString);
	}

	expect(await reader.done()).toBe(true);
});

test("Reader from stream", async () => {
	const chunks = [new Uint8Array([1, 2, 3]), new Uint8Array([4, 5]), new Uint8Array([6, 7, 8, 9])];

	const stream = new ReadableStream({
		start(controller) {
			for (const chunk of chunks) {
				controller.enqueue(chunk);
			}
			controller.close();
		},
	});

	const reader = new Reader(stream);

	// Read all data
	const result = await reader.readAll();
	expect(result).toEqual(new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9]));
});

test("Reader stream with partial reads", async () => {
	const chunks = [new Uint8Array([1, 2]), new Uint8Array([3, 4, 5]), new Uint8Array([6])];

	const stream = new ReadableStream({
		start(controller) {
			for (const chunk of chunks) {
				controller.enqueue(chunk);
			}
			controller.close();
		},
	});

	const reader = new Reader(stream);

	// Read specific amounts that cross chunk boundaries
	const first = await reader.read(3); // Should span first two chunks
	expect(first).toEqual(new Uint8Array([1, 2, 3]));

	const second = await reader.read(2);
	expect(second).toEqual(new Uint8Array([4, 5]));

	const third = await reader.read(1);
	expect(third).toEqual(new Uint8Array([6]));

	expect(await reader.done()).toBe(true);
});

test("Reader u53 decodes two-byte stream type prefixes", async () => {
	const { stream, written } = createTestWritableStream();
	const writer = new Writer(stream);

	await writer.u53(0x40);
	writer.close();
	await writer.closed;

	const reader = new Reader(undefined, concatChunks(written));
	expect(await reader.u53()).toBe(0x40);
	expect(await reader.done()).toBe(true);
});

/** A stream reset as a transport delivers one: the peer's code, and nothing else useful. */
class Reset extends Error {
	readonly source = "stream" as const;
	readonly streamErrorCode: number;

	constructor(code: number) {
		super("");
		this.streamErrorCode = code;
	}
}

test("Reader closed rejects with the decoded reset code", async () => {
	// Protocol paths race `closed` against a read, so the two must agree on the error
	// shape: whichever wins, the reset code reaches the caller the same way.
	const reader = new Reader(
		new ReadableStream<Uint8Array>({
			start: (controller) => controller.error(new Reset(2)),
		}),
	);

	const err = await reader.closed.then(
		() => undefined,
		(e: unknown) => e,
	);
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.DeliveryTimeout);
});

test("Writer closed rejects with the decoded reset code", async () => {
	const writer = new Writer(
		new WritableStream<Uint8Array>({
			start: (controller) => controller.error(new Reset(31)),
		}),
	);

	const err = await writer.closed.then(
		() => undefined,
		(e: unknown) => e,
	);
	expect(err).toBeInstanceOf(StreamError);
	expect(Number((err as StreamError).code)).toBe(31);
});

test("Writer reset forwards only a stream error code", async () => {
	const session = new SessionError(SessionCode.Unauthorized);
	const sessionWriter = new Writer(new WritableStream<Uint8Array>());
	sessionWriter.reset(session);
	const sessionResult = await sessionWriter.closed.then(
		() => undefined,
		(err: unknown) => err,
	);
	expect(sessionResult).toBe(session);

	const stream = new StreamError(StreamCode.DeliveryTimeout);
	const streamWriter = new Writer(new WritableStream<Uint8Array>());
	streamWriter.reset(stream);
	const streamResult = await streamWriter.closed.then(
		() => undefined,
		(err: unknown) => err,
	);
	expect(streamResult).toBeInstanceOf(StreamError);
	expect((streamResult as StreamError).code).toBe(StreamCode.DeliveryTimeout);
});

test("closed is stable, so racing it per frame does not allocate", async () => {
	const reader = new Reader(new ReadableStream<Uint8Array>());
	expect(reader.closed).toBe(reader.closed);
});

// Deadlines for the stalled fixtures below: one they always blow through, and one they
// never reach because the slot frees first. Neither is a delay any test waits out.
const EXPIRES_MS = 10;
const OUTLASTS_TEST_MS = 1000;

// Builds a transport whose uni opens complete only once `freeSlot` is called, standing in
// for a peer that has granted no stream credit. `aborted` settles when the stream it
// eventually hands over is reset, so a test can await that rather than guess at a delay.
function stalledTransport() {
	let freeSlot!: () => void;
	const slot = new Promise<void>((resolve) => {
		freeSlot = resolve;
	});

	let abort!: (reason: unknown) => void;
	const aborted = new Promise<unknown>((resolve) => {
		abort = resolve;
	});

	const stream = new WritableStream<Uint8Array>({ abort });

	const quic = {
		createUnidirectionalStream: async () => {
			await slot;
			return stream;
		},
	} as unknown as WebTransport;

	return { quic, freeSlot, aborted };
}

// The bidirectional counterpart, whose `discarded` settles when a stream handed over after
// the deadline is thrown away.
function stalledBidiTransport() {
	let freeSlot!: () => void;
	const slot = new Promise<void>((resolve) => {
		freeSlot = resolve;
	});

	let discard!: (reason: unknown) => void;
	const discarded = new Promise<unknown>((resolve) => {
		discard = resolve;
	});

	const stream = {
		readable: new ReadableStream<Uint8Array>({ cancel: discard }),
		writable: new WritableStream<Uint8Array>({ abort: discard }),
	};

	const quic = {
		createBidirectionalStream: async () => {
			await slot;
			return stream;
		},
	} as unknown as WebTransport;

	return { quic, freeSlot, discarded };
}

// Bidi opens have no group to drop, so they fail the way they did before
// waitUntilAvailable rather than parking for as long as the peer withholds credit.
test("open gives up on a peer that never frees a bidi slot", async () => {
	const { quic, freeSlot, discarded } = stalledBidiTransport();

	await expect(Stream.open(quic, { timeout: EXPIRES_MS })).rejects.toThrow(/timed out/);

	freeSlot();
	await discarded;
});

test("open waits for a bidi slot rather than failing on a busy session", async () => {
	const { quic, freeSlot } = stalledBidiTransport();

	const opening = Stream.open(quic, { timeout: OUTLASTS_TEST_MS });
	expect(await Promise.race([opening.then(() => "opened"), Promise.resolve("waiting")])).toBe("waiting");

	freeSlot();
	expect(await opening).toBeInstanceOf(Stream);
});

// A cancel that already settled has to win even when a slot is free, or a group whose
// subscriber is long gone still goes out.
test("tryOpen gives up on a cancel that settled before the open", async () => {
	const { quic, freeSlot, aborted } = stalledTransport();
	freeSlot();

	expect(await Writer.tryOpen(quic, { cancel: Promise.resolve() })).toBeUndefined();
	await aborted;
});

test("tryOpen gives up when cancelled, resetting a stream that opens afterwards", async () => {
	const { quic, freeSlot, aborted } = stalledTransport();

	let cancel!: () => void;
	const cancelled = new Promise<void>((resolve) => {
		cancel = resolve;
	});

	const opening = Writer.tryOpen(quic, { cancel: cancelled });
	cancel();
	expect(await opening).toBeUndefined();

	freeSlot();
	await aborted;
});

// Without a deadline a peer that withholds stream credit while keeping the subscription
// open queues work forever, since waitUntilAvailable never rejects.
test("tryOpen gives up when the peer never frees a slot", async () => {
	const { quic, freeSlot, aborted } = stalledTransport();

	expect(await Writer.tryOpen(quic, { cancel: new Promise(() => {}), timeout: EXPIRES_MS })).toBeUndefined();

	freeSlot();
	await aborted;
});

test("tryOpen returns the stream when a slot is available", async () => {
	const { quic, freeSlot } = stalledTransport();

	const opening = Writer.tryOpen(quic, { cancel: new Promise(() => {}), timeout: OUTLASTS_TEST_MS });
	freeSlot();

	expect(await opening).toBeInstanceOf(Writer);
});

test("open waits for a stream slot instead of rejecting once the peer's limit is full", async () => {
	const options: unknown[] = [];
	const quic = {
		createBidirectionalStream: async (opts?: unknown) => {
			options.push(opts);
			return { readable: new ReadableStream<Uint8Array>(), writable: new WritableStream<Uint8Array>() };
		},
		createUnidirectionalStream: async (opts?: unknown) => {
			options.push(opts);
			return new WritableStream<Uint8Array>();
		},
	} as unknown as WebTransport;

	await Stream.open(quic, { sendOrder: 7 });
	await Writer.open(quic);
	// The one path that opens faster than a peer can retire streams opts out.
	await Writer.open(quic, { waitUntilAvailable: false });

	expect(options).toEqual([
		{ sendOrder: 7, waitUntilAvailable: true },
		{ sendOrder: undefined, waitUntilAvailable: true },
		{ sendOrder: undefined, waitUntilAvailable: false },
	]);
});
