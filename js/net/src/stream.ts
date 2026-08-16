import { fromTransport, RemoteError, StreamCode, toTransport } from "./error.ts";
import type { IetfVersion } from "./ietf/version.ts";
import { Version } from "./ietf/version.ts";
import { TimeoutError, withTimeout } from "./util/timeout.ts";
import * as Varint from "./varint.ts";

// Forwarding a peer's reset must keep its code, or a relay flattens it to 0 (INTERNAL_ERROR).
// Anything else is a local failure, which 0 already describes, so leave it alone.
function withCode(reason: unknown): unknown {
	return reason instanceof RemoteError ? toTransport(reason.code, reason.message) : reason;
}

const MAX_U31 = 2 ** 31 - 1;
const MAX_READ_SIZE = 1024 * 1024 * 64; // don't allocate more than 64MB for a message

/**
 * Options handed to the transport for one outgoing stream.
 *
 * `waitUntilAvailable` waits for the peer's concurrent stream limit to free a slot instead
 * of rejecting with a `QuotaExceededError`, which is what we want for the streams a session
 * opens occasionally. `@types/web` doesn't declare it yet, hence the intersection.
 */
export function sendOptions(options?: OpenOptions): WebTransportSendStreamOptions & { waitUntilAvailable?: boolean } {
	return { sendOrder: options?.sendOrder, waitUntilAvailable: options?.waitUntilAvailable ?? true };
}

// How long any open may wait for the peer to free a stream slot. Every open needs this,
// whether or not it asked to wait: an implementation may park an over-limit open rather
// than rejecting it, and a peer can advertise a stream limit of zero and never raise it.
// Matches the subscribe budget.
const OPEN_TIMEOUT_MS = 10_000;

/**
 * Bound an open that would otherwise park until the peer grants stream credit, restoring
 * the pre-waitUntilAvailable failure after a grace period. A stream arriving after the
 * deadline is discarded, since nobody is waiting for it any more.
 */
async function openWithin<T>(opening: Promise<T>, timeout: number, discard: (stream: T) => void): Promise<T> {
	try {
		return await withTimeout(opening, timeout, `stream open timed out after ${timeout}ms waiting for a slot`);
	} catch (err: unknown) {
		opening.then(discard).catch(() => void 0);
		throw err;
	}
}

function isLeadingOnes(version?: IetfVersion): boolean {
	return (
		version !== undefined &&
		version !== Version.DRAFT_14 &&
		version !== Version.DRAFT_15 &&
		version !== Version.DRAFT_16
	);
}

/**
 * The `WebTransportSendStream` that every outgoing stream is, narrowed to the one attribute
 * this package uses. The DOM types available here still describe them as plain
 * `WritableStream`s, so the interface has to be named structurally.
 *
 * `sendOrder` is optional because it is absent until set, and stays absent on an
 * implementation that doesn't carry the interface at all.
 *
 * @see https://www.w3.org/TR/webtransport/#webtransportsendstream
 */
export type SendStream = WritableStream<Uint8Array> & { sendOrder?: number };

/** Options for opening an outgoing stream. */
export interface OpenOptions {
	/** The negotiated IETF version, which selects the varint encoding. */
	version?: IetfVersion;

	/**
	 * The transport send order, where HIGHER values are transmitted first.
	 * Left to the transport's default when unset.
	 */
	sendOrder?: number;

	/**
	 * Reject if the peer hasn't freed a stream slot within this many milliseconds. Defaults
	 * to 10s. There is no way to wait indefinitely: a peer can advertise a stream limit of
	 * zero and never raise it, so every open needs a way out.
	 */
	timeout?: number;

	/**
	 * Ask the transport to wait for a stream slot rather than failing when the peer's
	 * concurrent stream limit is exhausted. Defaults to true.
	 *
	 * Set it false on a path that opens streams faster than the peer can retire them. The
	 * transport queues the opens it can't satisfy and serves them in order, with no way to
	 * cancel one, so waiting there spends returning credit on whatever was requested first
	 * rather than on what matters now.
	 */
	waitUntilAvailable?: boolean;
}

/** Options for {@link Writer.tryOpen}. */
export interface TryOpenOptions extends OpenOptions {
	/** Give up once this settles, however it settles. */
	cancel: Promise<unknown>;
}

export class Stream {
	reader: Reader;
	writer: Writer;

	/** Wrap the two halves of a transport stream. */
	constructor(props: {
		writable: WritableStream<Uint8Array>;
		readable: ReadableStream<Uint8Array>;
		version?: IetfVersion;
	});
	/** Pair halves that were opened separately, as the SETUP exchange does. */
	constructor(props: { writer: Writer; reader: Reader });
	constructor(props: {
		writable?: WritableStream<Uint8Array>;
		readable?: ReadableStream<Uint8Array>;
		writer?: Writer;
		reader?: Reader;
		version?: IetfVersion;
	}) {
		const writer = props.writer ?? (props.writable && new Writer(props.writable, props.version));
		const reader = props.reader ?? (props.readable && new Reader(props.readable, undefined, props.version));
		if (!writer || !reader) throw new Error("stream needs both halves");

		this.writer = writer;
		this.reader = reader;
	}

	static async accept(quic: WebTransport, version?: IetfVersion): Promise<Stream | undefined> {
		for (;;) {
			const reader =
				quic.incomingBidirectionalStreams.getReader() as ReadableStreamDefaultReader<WebTransportBidirectionalStream>;
			const next = await reader.read();
			reader.releaseLock();

			if (next.done) return;
			const { readable, writable } = next.value;
			return new Stream({ readable, writable, version });
		}
	}

	/**
	 * Open an outgoing bidirectional stream.
	 * @param quic - The session to open it on
	 * @param options - The version its varints encode with, and the send order ranking it
	 *   against the session's other streams
	 */
	static async open(quic: WebTransport, options?: OpenOptions): Promise<Stream> {
		const { readable, writable } = await openWithin(
			quic.createBidirectionalStream(sendOptions(options)),
			options?.timeout ?? OPEN_TIMEOUT_MS,
			(stream) => {
				void stream.writable.abort().catch(() => void 0);
				void stream.readable.cancel().catch(() => void 0);
			},
		);
		return new Stream({ readable, writable, version: options?.version });
	}

	close() {
		this.writer.close();
		// A routine unsubscribe, so send CANCELLED. A bare Error would put 0 on the wire,
		// which the stream registry reads as INTERNAL_ERROR: the peer would log a failure
		// for every subscription we walk away from.
		this.reader.stop(toTransport(StreamCode.Cancel, "cancel"));
	}

	abort(reason: Error) {
		this.writer.reset(reason);
		this.reader.stop(reason);
	}
}

// Reader wraps a stream and provides convience methods for reading pieces from a stream
// Unfortunately we can't use a BYOB reader because it's not supported with WebTransport+WebWorkers yet.
export class Reader {
	#buffer: Uint8Array;
	#stream?: ReadableStream<Uint8Array>; // if undefined, the buffer is consumed then EOF
	#reader?: ReadableStreamDefaultReader<Uint8Array>;
	#closed?: Promise<void>;
	version?: IetfVersion;

	// Either stream or buffer MUST be provided.
	constructor(stream: ReadableStream<Uint8Array>, buffer?: Uint8Array, version?: IetfVersion);
	constructor(stream: undefined, buffer: Uint8Array, version?: IetfVersion);
	constructor(stream?: ReadableStream<Uint8Array>, buffer?: Uint8Array, version?: IetfVersion) {
		this.#buffer = buffer ?? new Uint8Array();
		this.#stream = stream;
		this.#reader = this.#stream?.getReader();
		this.version = version;
	}

	// Adds more data to the buffer, returning true if more data was added.
	async #fill(): Promise<boolean> {
		if (!this.#reader) {
			return false;
		}

		// Every read of this stream funnels through here, so decoding the peer's reset code
		// once is enough to keep the raw transport error out of every caller (and every app).
		const result = await this.#reader.read().catch((err: unknown) => {
			throw fromTransport(err);
		});

		if (result.done) {
			return false;
		}

		if (result.value.byteLength === 0) {
			throw new Error("unexpected empty chunk");
		}

		const buffer = new Uint8Array(result.value);

		if (this.#buffer.byteLength === 0) {
			this.#buffer = buffer;
		} else {
			const temp = new Uint8Array(this.#buffer.byteLength + buffer.byteLength);
			temp.set(this.#buffer);
			temp.set(buffer, this.#buffer.byteLength);
			this.#buffer = temp;
		}

		return true;
	}

	// Add more data to the buffer until it's at least size bytes.
	async #fillTo(size: number) {
		if (size > MAX_READ_SIZE) {
			throw new Error(`read size ${size} exceeds max size ${MAX_READ_SIZE}`);
		}

		while (this.#buffer.byteLength < size) {
			if (!(await this.#fill())) {
				throw new Error("unexpected end of stream");
			}
		}
	}

	// Consumes the first size bytes of the buffer.
	#slice(size: number): Uint8Array {
		const result = new Uint8Array(this.#buffer.buffer, this.#buffer.byteOffset, size);
		this.#buffer = new Uint8Array(
			this.#buffer.buffer,
			this.#buffer.byteOffset + size,
			this.#buffer.byteLength - size,
		);

		return result;
	}

	async read(size: number): Promise<Uint8Array> {
		if (size === 0) return new Uint8Array();

		await this.#fillTo(size);
		return this.#slice(size);
	}

	async readAll(): Promise<Uint8Array> {
		while (await this.#fill()) {
			// keep going
		}
		return this.#slice(this.#buffer.byteLength);
	}

	async string(): Promise<string> {
		const length = await this.u53();
		const buffer = await this.read(length);
		return new TextDecoder().decode(buffer);
	}

	async bool(): Promise<boolean> {
		const v = await this.u8();
		if (v === 0) return false;
		if (v === 1) return true;
		throw new Error("invalid bool value");
	}

	async u8(): Promise<number> {
		await this.#fillTo(1);
		return this.#slice(1)[0];
	}

	async u16(): Promise<number> {
		await this.#fillTo(2);
		const view = new DataView(this.#buffer.buffer, this.#buffer.byteOffset, 2);
		const result = view.getUint16(0);
		this.#slice(2);
		return result;
	}

	// Returns a Number using 53-bits, the max Javascript can use for integer math.
	// Values > 2^53-1 are coerced to a Number (precision is lost) and logged. We
	// downgrade overflow from throw to warn so a stray u64 field on the wire (e.g.
	// a peer's session-level Origin id) doesn't tear down the whole stream/session.
	async u53(): Promise<number> {
		const v = await this.u62();
		if (v > Varint.MAX_U53) {
			console.warn(`value larger than 53-bits; use u62 instead (precision lost): ${v.toString()}`);
		}

		return Number(v);
	}

	// NOTE: Returns a bigint instead of a number since it may be larger than 53-bits
	async u62(): Promise<bigint> {
		if (isLeadingOnes(this.version)) {
			return this.#readLeadingOnes();
		}
		return this.#readQuicVarint();
	}

	async #readQuicVarint(): Promise<bigint> {
		await this.#fillTo(1);
		const size = (this.#buffer[0] & 0xc0) >> 6;

		if (size === 0) {
			const first = this.#slice(1)[0];
			return BigInt(first) & 0x3fn;
		}
		if (size === 1) {
			await this.#fillTo(2);
			const slice = this.#slice(2);
			const view = new DataView(slice.buffer, slice.byteOffset, slice.byteLength);

			return BigInt(view.getUint16(0)) & 0x3fffn;
		}
		if (size === 2) {
			await this.#fillTo(4);
			const slice = this.#slice(4);
			const view = new DataView(slice.buffer, slice.byteOffset, slice.byteLength);

			return BigInt(view.getUint32(0)) & 0x3fffffffn;
		}
		await this.#fillTo(8);
		const slice = this.#slice(8);
		const view = new DataView(slice.buffer, slice.byteOffset, slice.byteLength);

		return view.getBigUint64(0) & 0x3fffffffffffffffn;
	}

	async #readLeadingOnes(): Promise<bigint> {
		await this.#fillTo(1);
		const b = this.#buffer[0];

		// Count leading 1-bits
		let ones = 0;
		for (let bit = 7; bit >= 0; bit--) {
			if (b & (1 << bit)) ones++;
			else break;
		}

		// 1111110x is a 7-byte form. Draft-17 rejects it; draft-18+ allows it per #1595.
		if (ones === 6 && this.version === Version.DRAFT_17) {
			throw new Error("invalid leading-ones varint: 1111110x prefix is reserved on draft-17");
		}

		let totalSize: number;
		if (ones <= 5) totalSize = ones + 1;
		else if (ones === 6) totalSize = 7;
		else if (ones === 7) totalSize = 8;
		else totalSize = 9; // ones === 8

		await this.#fillTo(totalSize);
		const slice = this.#slice(totalSize);

		const [value] = Varint.decodeLeadingOnes(slice);
		return value;
	}

	// Returns false if there is more data to read, blocking if it hasn't been received yet.
	async done(): Promise<boolean> {
		if (this.#buffer.byteLength > 0) return false;
		return !(await this.#fill());
	}

	stop(reason: unknown) {
		this.#reader?.cancel(withCode(reason)).catch(() => void 0);
	}

	// Decoded like #fill: a caller racing this against a read must not get a different error
	// shape depending on which one won. Derived once, so racing it per frame doesn't allocate.
	get closed(): Promise<void> {
		this.#closed ??= (this.#reader?.closed ?? Promise.resolve()).catch((err: unknown) => {
			throw fromTransport(err);
		});
		return this.#closed;
	}
}

// Writer wraps a stream and writes chunks of data
export class Writer {
	#writer: WritableStreamDefaultWriter<Uint8Array>;
	#stream: WritableStream<Uint8Array>;
	#closed?: Promise<void>;

	// Scratch buffer for writing varints.
	// Fixed at 9 bytes (leading-ones max).
	#scratch: ArrayBuffer;

	version?: IetfVersion;

	constructor(stream: WritableStream<Uint8Array>, version?: IetfVersion) {
		this.#stream = stream;
		this.#scratch = new ArrayBuffer(9);
		this.#writer = this.#stream.getWriter();
		this.version = version;
	}

	/**
	 * Rank this stream against the session's others, where HIGHER values are sent first.
	 *
	 * A send order only schedules the local end, so a stream the peer opened has to be ranked
	 * here rather than at the peer's {@link open}.
	 *
	 * The spec makes `sendOrder` a settable attribute on every {@link SendStream}. Where the
	 * interface isn't implemented (Chrome as of writing, a mock, a polyfill) this just sets an
	 * ignored property, the same way an ignored `sendOrder` option does at {@link open}.
	 */
	setPriority(sendOrder: number) {
		(this.#stream as SendStream).sendOrder = sendOrder;
	}

	async bool(v: boolean) {
		await this.write(setUint8(this.#scratch, v ? 1 : 0));
	}

	async u8(v: number) {
		await this.write(setUint8(this.#scratch, v));
	}

	async u16(v: number) {
		await this.write(setUint16(this.#scratch, v));
	}

	async i32(v: number) {
		if (Math.abs(v) > MAX_U31) {
			throw new Error(`overflow, value larger than 32-bits: ${v.toString()}`);
		}

		// We don't use a VarInt, so it always takes 4 bytes.
		// This could be improved but nothing is standardized yet.
		await this.write(setInt32(this.#scratch, v));
	}

	async u53(v: number) {
		if (v > Varint.MAX_U53) {
			// Number values above 2^53-1 have already lost precision before reaching
			// the wire, but downgrade overflow to warn so an upstream miscount
			// doesn't tear down the whole stream. The encoded varint will reflect
			// the truncated Number value.
			console.warn(`value larger than 53-bits; use u62 instead (precision lost): ${v.toString()}`);
		}
		if (isLeadingOnes(this.version)) {
			await this.write(Varint.encodeLeadingOnesTo(this.#scratch, v));
		} else {
			await this.write(Varint.encodeTo(this.#scratch, v));
		}
	}

	async u62(v: bigint) {
		if (isLeadingOnes(this.version)) {
			await this.write(Varint.encodeLeadingOnesTo(this.#scratch, v));
		} else {
			await this.write(Varint.encodeTo(this.#scratch, v));
		}
	}

	async write(v: Uint8Array) {
		// Mirrors Reader.#fill: every write funnels through here, so a STOP_SENDING from the
		// peer surfaces as a typed code rather than the transport's own error shape.
		await this.#writer.write(v).catch((err: unknown) => {
			throw fromTransport(err);
		});
	}

	async string(str: string) {
		const data = new TextEncoder().encode(str);
		await this.u53(data.byteLength);
		await this.write(data);
	}

	close() {
		this.#writer.close().catch(() => void 0);
	}

	// Mirrors Reader.closed: a STOP_SENDING reaches a caller racing this with the same
	// typed code it would get from a write.
	get closed(): Promise<void> {
		this.#closed ??= this.#writer.closed.catch((err: unknown) => {
			throw fromTransport(err);
		});
		return this.#closed;
	}

	reset(reason: unknown) {
		this.#writer.abort(withCode(reason)).catch(() => void 0);
	}

	/**
	 * Open an outgoing unidirectional stream.
	 * @param quic - The session to open it on
	 * @param options - The version its varints encode with, and the send order ranking it
	 *   against the session's other streams
	 */
	static async open(quic: WebTransport, options?: OpenOptions): Promise<Writer> {
		const writable = await openWithin(
			quic.createUnidirectionalStream(sendOptions(options)) as Promise<WritableStream<Uint8Array>>,
			options?.timeout ?? OPEN_TIMEOUT_MS,
			(stream) => void stream.abort().catch(() => void 0),
		);

		return new Writer(writable, options?.version);
	}

	/**
	 * Like {@link Writer.open}, but gives up when `cancel` settles or `timeout` elapses,
	 * returning undefined so the caller can drop whatever it meant to send. A stream that
	 * opens after that is reset rather than leaked. A real transport failure still throws.
	 *
	 * Worth using even with `waitUntilAvailable: false`, since an implementation may park
	 * an over-limit open instead of rejecting it.
	 */
	static async tryOpen(quic: WebTransport, options: TryOpenOptions): Promise<Writer | undefined> {
		// A rejected `cancel` (STOP_SENDING) means the peer is gone too, so both settle
		// paths mean "give up" and neither is left unhandled. Built before the open, and
		// raced ahead of it, so an already-cancelled caller wins even against a slot that
		// is free right now.
		const cancelled = options.cancel.then(
			() => undefined,
			() => undefined,
		);
		const open = Writer.open(quic, options);

		try {
			const stream = await Promise.race([cancelled, open]);
			if (stream) return stream;
		} catch (err: unknown) {
			// open already discarded the late stream on its way out.
			if (!(err instanceof TimeoutError)) throw err;
			return undefined;
		}

		const abandoned = new Error("abandoned waiting for a stream slot");
		open.then((w) => w.reset(abandoned)).catch(() => void 0);
		return undefined;
	}
}

function setUint8(dst: ArrayBuffer, v: number): Uint8Array {
	const buffer = new Uint8Array(dst, 0, 1);
	buffer[0] = v;
	return buffer;
}

function setUint16(dst: ArrayBuffer, v: number): Uint8Array {
	const view = new DataView(dst, 0, 2);
	view.setUint16(0, v);
	return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}

function setInt32(dst: ArrayBuffer, v: number): Uint8Array {
	const view = new DataView(dst, 0, 4);
	view.setInt32(0, v);
	return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}

// Returns the next stream from the connection
export class Readers {
	#reader: ReadableStreamDefaultReader<ReadableStream<Uint8Array>>;
	#version?: IetfVersion;

	constructor(quic: WebTransport, version?: IetfVersion) {
		this.#reader = quic.incomingUnidirectionalStreams.getReader() as ReadableStreamDefaultReader<
			ReadableStream<Uint8Array>
		>;
		this.#version = version;
	}

	async next(): Promise<Reader | undefined> {
		const next = await this.#reader.read();
		if (next.done) return;
		return new Reader(next.value, undefined, this.#version);
	}

	close() {
		this.#reader.cancel();
	}
}
