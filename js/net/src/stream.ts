import { race } from "@moq/signals";
import { fromTransport, StreamCode, toStreamCode, toTransport } from "./error.ts";
import { sharedStreamCode } from "./ietf/error.ts";
import type { IetfVersion } from "./ietf/version.ts";
import { Version } from "./ietf/version.ts";
import { Version as Lite, type Version as LiteVersion } from "./lite/version.ts";
import { TimeoutError, withTimeout } from "./util/timeout.ts";
import { POW32, toBigInt, toNumber, U64 } from "./util/u64.ts";
import { decodeUtf8 } from "./util/utf8.ts";
import {
	lengthLeadingOnes,
	lengthQuic,
	parts,
	peekLeadingOnes,
	peekQuic,
	readLeadingOnes,
	readQuic,
	split,
	writeLeadingOnes,
	writeQuic,
} from "./util/varint.ts";

// Decode raw transport errors before mapping so they cannot bypass the negotiated
// registry. Ordinary errors already send 0 and retain their local identity.
// A bare stream code takes the same check, without building a StreamError first.
function withCode(reason: unknown, stream: StreamVersion): unknown {
	const version = asIetf(stream);
	if (typeof reason === "number") {
		const code = version === undefined || sharedStreamCode(reason, version) ? reason : StreamCode.Internal;
		return toTransport(code as StreamCode, "cancel");
	}
	const decoded = fromTransport(reason, { version });
	const code = toStreamCode(decoded, { version });
	return code === StreamCode.Internal && decoded === reason ? reason : toTransport(code, decoded.message);
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

/**
 * The version a stream's bytes follow: a moq-transport draft or a moq-lite draft. Required on
 * every stream so the varint form never defaults silently; a stream opened before negotiation
 * names the version its handshake is encoded with.
 */
export type StreamVersion = IetfVersion | LiteVersion;

const LITE: ReadonlySet<number> = new Set(Object.values(Lite));

function isLite(version: StreamVersion): version is LiteVersion {
	return LITE.has(version);
}

/** The moq-transport draft a stream follows, or undefined on moq-lite, whose stream codes are the same on every draft. */
export function asIetf(version: StreamVersion): IetfVersion | undefined {
	return isLite(version) ? undefined : version;
}

// Every draft newer than these counts leading ones, so a new version falls forward.
function isLeadingOnes(version: StreamVersion): boolean {
	switch (version) {
		case Version.DRAFT_14:
		case Version.DRAFT_15:
		case Version.DRAFT_16:
		case Lite.DRAFT_01:
		case Lite.DRAFT_02:
		case Lite.DRAFT_03:
		case Lite.DRAFT_04:
		case Lite.DRAFT_05:
		case Lite.DRAFT_06:
			return false;
		default:
			return true;
	}
}

// Encode `hi`/`lo` into `dst` in the varint form `version` uses: QUIC up to 2^62-1, leading-ones up to 2^64-1.
function encodeTo(dst: ArrayBuffer, hi: number, lo: number, version: StreamVersion): Uint8Array {
	let buf: Uint8Array;
	if (isLeadingOnes(version)) {
		buf = new Uint8Array(dst, 0, lengthLeadingOnes(hi, lo));
		writeLeadingOnes(buf, hi, lo, buf.length);
	} else {
		buf = new Uint8Array(dst, 0, lengthQuic(hi, lo));
		writeQuic(buf, hi, lo, buf.length);
	}
	return buf;
}

/** Encode one varint in the form `version` uses, for a body written outside a {@link Writer}. */
export function encodeVarint(v: number | bigint, version: StreamVersion): Uint8Array {
	const lo = split(v);
	return encodeTo(new ArrayBuffer(9), parts.hi, lo, version);
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
	/** The negotiated version, which selects the varint encoding. */
	version: StreamVersion;

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
	/** Give up once this resolves. */
	cancel: Promise<void>;
}

export class Stream {
	reader: Reader;
	writer: Writer;

	/** Wrap the two halves of a transport stream. */
	constructor(props: {
		writable: WritableStream<Uint8Array>;
		readable: ReadableStream<Uint8Array>;
		version: StreamVersion;
	});
	/** Pair halves that were opened separately, as the SETUP exchange does. */
	constructor(props: { writer: Writer; reader: Reader });
	constructor(props: {
		writable?: WritableStream<Uint8Array>;
		readable?: ReadableStream<Uint8Array>;
		writer?: Writer;
		reader?: Reader;
		version?: StreamVersion;
	}) {
		const version = props.version;
		const writer = props.writer ?? (props.writable && version !== undefined && new Writer(props.writable, version));
		const reader =
			props.reader ?? (props.readable && version !== undefined && new Reader(props.readable, undefined, version));
		if (!writer || !reader) throw new Error("stream needs both halves");

		this.writer = writer;
		this.reader = reader;
	}

	static async accept(quic: WebTransport, version: StreamVersion): Promise<Stream | undefined> {
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
	static async open(quic: WebTransport, options: OpenOptions): Promise<Stream> {
		const { readable, writable } = await openWithin(
			quic.createBidirectionalStream(sendOptions(options)),
			options?.timeout ?? OPEN_TIMEOUT_MS,
			(stream) => {
				void stream.writable.abort().catch(() => void 0);
				void stream.readable.cancel().catch(() => void 0);
			},
		);
		return new Stream({ readable, writable, version: options.version });
	}

	close() {
		this.writer.close();
		// A routine unsubscribe, so send CANCELLED. A bare Error would put 0 on the wire,
		// which the stream registry reads as INTERNAL_ERROR: the peer would log a failure
		// for every subscription we walk away from.
		this.reader.stop(StreamCode.Cancel);
	}

	abort(reason: Error) {
		this.writer.reset(reason);
		this.reader.stop(reason);
	}
}

// Reader wraps a stream and provides convience methods for reading pieces from a stream
// Unfortunately we can't use a BYOB reader because it's not supported with WebTransport+WebWorkers yet.
export class Reader {
	// Contiguous unread bytes, followed by chunks not yet joined onto it. Joining only once a
	// read needs the bytes keeps a frame arriving in N chunks linear rather than quadratic.
	#buffer: Uint8Array;
	#chunks: Uint8Array[] = [];
	#chunked = 0; // bytes across #chunks
	#stream?: ReadableStream<Uint8Array>; // if undefined, the buffer is consumed then EOF
	#reader?: ReadableStreamDefaultReader<Uint8Array>;
	#closed?: Promise<void>;
	// The decode that last ran short and how far, so a retry can wait for those bytes.
	#short?: { decode: (c: Cursor) => unknown; err: Short };
	version: StreamVersion;

	// Either stream or buffer MUST be provided.
	constructor(stream: ReadableStream<Uint8Array>, buffer: Uint8Array | undefined, version: StreamVersion);
	constructor(stream: undefined, buffer: Uint8Array, version: StreamVersion);
	constructor(
		stream: ReadableStream<Uint8Array> | undefined,
		buffer: Uint8Array | undefined,
		version: StreamVersion,
	) {
		this.#buffer = buffer ?? new Uint8Array();
		this.#stream = stream;
		this.#reader = this.#stream?.getReader();
		this.version = version;
	}

	// Adds more data to the buffer, returning true if more data was added.
	async #fill(): Promise<boolean> {
		const reader = this.#reader;
		if (!reader) {
			return false;
		}

		// Every read of this stream funnels through here, so decoding the peer's reset code
		// once is enough to keep the raw transport error out of every caller (and every app).
		const result = await reader.read().catch((err: unknown) => {
			throw fromTransport(err, { version: asIetf(this.version) });
		});

		if (result.done) {
			// The transport already finished the stream. Drop the reader so a later stop
			// neither builds a cancel error nor sends a reset the peer will ignore.
			if (this.#reader === reader) {
				this.#reader = undefined;
				reader.releaseLock();
			}
			return false;
		}

		if (result.value.byteLength === 0) {
			throw new Error("unexpected empty chunk");
		}

		this.#chunks.push(result.value);
		this.#chunked += result.value.byteLength;

		return true;
	}

	// Add more data to the buffer until it's at least size bytes.
	async #fillTo(size: number) {
		if (size > MAX_READ_SIZE) {
			throw new Error(`read size ${size} exceeds max size ${MAX_READ_SIZE}`);
		}

		if (this.#buffer.byteLength >= size) return;

		while (this.#buffer.byteLength + this.#chunked < size) {
			if (!(await this.#fill())) {
				throw new UnexpectedEnd();
			}
		}

		this.#join();
	}

	// Move every pending chunk into the buffer, copying only when there's more than one piece.
	#join() {
		if (this.#chunks.length === 0) return;

		if (this.#buffer.byteLength === 0 && this.#chunks.length === 1) {
			this.#buffer = this.#chunks[0];
		} else {
			const joined = new Uint8Array(this.#buffer.byteLength + this.#chunked);
			joined.set(this.#buffer);
			let offset = this.#buffer.byteLength;
			for (const chunk of this.#chunks) {
				joined.set(chunk, offset);
				offset += chunk.byteLength;
			}
			this.#buffer = joined;
		}

		this.#chunks = [];
		this.#chunked = 0;
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

	/**
	 * Run a synchronous decode over the buffered bytes and consume what it read.
	 *
	 * Returns undefined and consumes nothing when the decode ran past the buffered bytes, so a
	 * caller can drain every complete message already here without waiting on the stream.
	 */
	tryDecode<T extends NonNullable<unknown>>(decode: (c: Cursor) => T): T | undefined {
		const result = this.#try(decode);
		return result instanceof Short ? undefined : result;
	}

	/** Run a synchronous decode, filling from the stream until it has the bytes it needs. */
	async decode<T>(decode: (c: Cursor) => T): Promise<T> {
		for (;;) {
			const result = this.#try(decode);
			if (!(result instanceof Short)) return result;
			await this.#fillTo(result.need);
		}
	}

	// Like decode, but leaves the bytes buffered for the next read.
	async #peek<T>(decode: (c: Cursor) => T): Promise<T> {
		for (;;) {
			const result = this.#try(decode, false);
			if (!(result instanceof Short)) return result;
			await this.#fillTo(result.need);
		}
	}

	/** Like {@link decode}, but returns undefined if the stream ends cleanly first. */
	async decodeMaybe<T>(decode: (c: Cursor) => T): Promise<T | undefined> {
		if (await this.done()) return undefined;
		return this.decode(decode);
	}

	#try<T>(decode: (c: Cursor) => T, consume = true): T | Short {
		// A retry of the decode that last ran short, before the bytes it needs have arrived,
		// would only throw again. Every decode reads at least a byte, so none can succeed on
		// an empty buffer either.
		const available = this.#buffer.byteLength + this.#chunked;
		if (available === 0) return EMPTY;
		if (decode === this.#short?.decode && available < this.#short.err.need) return this.#short.err;

		this.#join();
		const cursor = new Cursor(this.#buffer, this.version);
		try {
			const result = decode(cursor);
			if (consume) this.#slice(cursor.offset);
			this.#short = undefined;
			return result;
		} catch (err: unknown) {
			if (!(err instanceof Short)) throw err;
			// Filling could never satisfy it, so retrying would spin.
			if (err.need <= this.#buffer.byteLength) throw new Error("decode ran short of bytes it already had");
			this.#short = { decode, err };
			return err;
		}
	}

	async read(size: number): Promise<Uint8Array> {
		if (size === 0) return new Uint8Array();
		return this.decode((c) => c.read(size));
	}

	async readAll(): Promise<Uint8Array> {
		while (await this.#fill()) {
			// keep going
		}
		this.#join();
		return this.#slice(this.#buffer.byteLength);
	}

	// Reads to the end of the stream, dropping every byte instead of buffering it.
	async discard(): Promise<void> {
		this.#buffer = new Uint8Array();
		do {
			this.#chunks = [];
			this.#chunked = 0;
		} while (await this.#fill());
	}

	async string(): Promise<string> {
		return this.decode(STRING);
	}

	async bool(): Promise<boolean> {
		return this.decode(BOOL);
	}

	async u8(): Promise<number> {
		return this.decode(U8);
	}

	async u16(): Promise<number> {
		return this.decode(U16);
	}

	// Returns a Number using 53-bits, the max Javascript can use for integer math.
	async u53(): Promise<number> {
		return this.decode(U53);
	}

	// NOTE: Returns a bigint instead of a number since it may be larger than 53-bits
	async u62(): Promise<bigint> {
		return this.decode(U62);
	}

	/** Like {@link u62}, but leaves the varint buffered, so a stream's type can be read twice. */
	async peekU62(): Promise<bigint> {
		return this.#peek(U62);
	}

	async varint(): Promise<U64> {
		return this.decode(VARINT);
	}

	// Returns false if there is more data to read, blocking if it hasn't been received yet.
	async done(): Promise<boolean> {
		if (this.#buffer.byteLength > 0 || this.#chunked > 0) return false;
		return !(await this.#fill());
	}

	// The transport error is built only while the stream is still open. After a FIN the
	// reader is gone, so this allocates nothing and sends nothing.
	stop(reason: unknown) {
		const reader = this.#reader;
		if (!reader) return;
		this.#reader = undefined;
		reader.cancel(withCode(reason, this.version)).catch(() => void 0);
	}

	// Decoded like #fill: a caller racing this against a read must not get a different error
	// shape depending on which one won. Derived once, so racing it per frame doesn't allocate.
	get closed(): Promise<void> {
		this.#closed ??= (this.#reader?.closed ?? Promise.resolve()).catch((err: unknown) => {
			throw fromTransport(err, { version: asIetf(this.version) });
		});
		return this.#closed;
	}
}

/** The stream ended cleanly partway through a read. */
export class UnexpectedEnd extends Error {
	constructor() {
		super("unexpected end of stream");
	}
}

// Thrown by a Cursor read that runs past the buffered bytes, carrying how many bytes from the
// start of the buffer the decode needs. Not an Error: it ends every chunk, so it must not
// capture a stack.
class Short {
	readonly need: number;

	constructor(need: number) {
		this.need = need;
	}
}

const EMPTY = new Short(1);

/**
 * A synchronous view over a {@link Reader}'s buffered bytes, handed to {@link Reader.decode}.
 *
 * A read past the buffered bytes throws an internal signal that the Reader catches: it consumes
 * nothing, fills, and runs the decode again from the start. A decode must therefore not mutate
 * anything before its last read, must not swallow what it throws, and must read at least a byte.
 */
export class Cursor {
	readonly version: StreamVersion;
	#buffer: Uint8Array;
	#offset = 0;
	// Resolved once, since every varint read branches on it.
	#leadingOnes: boolean;
	// First bytes below this are a whole 1-byte varint, and below this + 0x40 a 2-byte one whose
	// value is the low 6 bits and the next byte. Both formats share that shape; only the bound moves.
	#short: number;
	// First bytes below this are a varint of at most 4 bytes: 0xc0 for QUIC, 0xf0 for leading-ones.
	#word: number;

	constructor(buffer: Uint8Array, version: StreamVersion) {
		this.#buffer = buffer;
		this.version = version;
		this.#leadingOnes = isLeadingOnes(version);
		this.#short = this.#leadingOnes ? 0x80 : 0x40;
		this.#word = this.#leadingOnes ? 0xf0 : 0xc0;
	}

	/** How many bytes have been read. */
	get offset(): number {
		return this.#offset;
	}

	/** How many buffered bytes are left to read. */
	get remaining(): number {
		return this.#buffer.byteLength - this.#offset;
	}

	/**
	 * Decode the next `size` bytes on their own. Running past them, or leaving any unread, is
	 * malformed rather than a reason to wait for more.
	 */
	exact<T>(size: number, decode: (c: Cursor) => T): T {
		const inner = new Cursor(this.read(size), this.version);
		let result: T;
		try {
			result = decode(inner);
		} catch (err: unknown) {
			if (err instanceof Short) throw new Error(`message is shorter than its fields: ${size} bytes`);
			throw err;
		}
		if (inner.remaining > 0) throw new Error(`message has ${inner.remaining} unread bytes`);
		return result;
	}

	#ensure(size: number) {
		const need = this.#offset + size;
		// Checked here too, and on the whole decode like the fill, since bytes that are already
		// buffered never reach the fill.
		if (need > MAX_READ_SIZE) throw new Error(`read size ${need} exceeds max size ${MAX_READ_SIZE}`);
		if (need > this.#buffer.byteLength) throw new Short(need);
	}

	/** Read `size` bytes, as a view onto the buffer rather than a copy. */
	read(size: number): Uint8Array {
		this.#ensure(size);
		const start = this.#offset;
		this.#offset += size;
		return this.#buffer.subarray(start, this.#offset);
	}

	string(): string {
		return decodeUtf8(this.read(this.u53()));
	}

	bool(): boolean {
		const v = this.u8();
		if (v === 0) return false;
		if (v === 1) return true;
		throw new Error("invalid bool value");
	}

	u8(): number {
		this.#ensure(1);
		return this.#buffer[this.#offset++];
	}

	u16(): number {
		this.#ensure(2);
		const b = this.#buffer;
		const o = this.#offset;
		this.#offset += 2;
		return (b[o] << 8) | b[o + 1];
	}

	/** Read a varint as a `number`, throwing if it is above `Number.MAX_SAFE_INTEGER`. */
	u53(): number {
		// Most varints are 1 or 2 bytes, which skip the general decode.
		this.#ensure(1);
		const b = this.#buffer;
		const o = this.#offset;
		const first = b[o];
		if (first < this.#short) {
			this.#offset = o + 1;
			return first;
		}
		if (first < this.#short + 0x40) {
			this.#ensure(2);
			this.#offset = o + 2;
			return ((first & 0x3f) << 8) | b[o + 1];
		}
		// Up to 4 bytes still fits 28 (leading-ones) or 30 (QUIC) bits, with no upper half.
		if (first < this.#word) {
			const size = this.#leadingOnes ? peekLeadingOnes(first) : 4;
			this.#ensure(size);
			this.#offset = o + size;
			if (size === 3) return ((first & 0x1f) << 16) | (b[o + 1] << 8) | b[o + 2];
			return ((first & (this.#leadingOnes ? 0x0f : 0x3f)) << 24) | (b[o + 1] << 16) | (b[o + 2] << 8) | b[o + 3];
		}
		const lo = this.#varint();
		return toNumber(parts.hi, lo);
	}

	/** Read a varint as a bigint. A leading-ones varint may exceed 62 bits. */
	u62(): bigint {
		const lo = this.#varint();
		return toBigInt(parts.hi, lo);
	}

	/** Read a varint. */
	varint(): U64 {
		const lo = this.#varint();
		return new U64(parts.hi, lo);
	}

	// Decode the next varint in the version's format, returning its lower half and leaving the upper in `parts`.
	#varint(): number {
		this.#ensure(1);
		const b = this.#buffer;
		const o = this.#offset;
		let size: number;
		if (this.#leadingOnes) {
			size = peekLeadingOnes(b[o]);
			// 1111110x is a 7-byte form. Draft-17 rejects it; draft-18+ allows it per #1595.
			if (size === 7 && this.version === Version.DRAFT_17) {
				throw new Error("invalid leading-ones varint: 1111110x prefix is reserved on draft-17");
			}
			this.#ensure(size);
			this.#offset += size;
			return readLeadingOnes(b, o, size);
		}
		size = peekQuic(b[o]);
		this.#ensure(size);
		this.#offset += size;
		return readQuic(b, o, size);
	}
}

// Shared decodes for the Reader's async primitives, so a read allocates no closure.
const STRING = (c: Cursor) => c.string();
const BOOL = (c: Cursor) => c.bool();
const U8 = (c: Cursor) => c.u8();
const U16 = (c: Cursor) => c.u16();
const U53 = (c: Cursor) => c.u53();
const U62 = (c: Cursor) => c.u62();
const VARINT = (c: Cursor) => c.varint();

// Writer wraps a stream and writes chunks of data
export class Writer {
	#writer: WritableStreamDefaultWriter<Uint8Array>;
	#stream: WritableStream<Uint8Array>;
	#closed?: Promise<void>;

	// Scratch buffer for each primitive write, sized for the longest (a 9-byte leading-ones varint).
	#scratch: ArrayBuffer;

	version: StreamVersion;

	constructor(stream: WritableStream<Uint8Array>, version: StreamVersion) {
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
		if (!Number.isInteger(v) || v < 0 || v > 255) {
			throw new RangeError(`invalid u8: ${v}`);
		}
		await this.write(setUint8(this.#scratch, v));
	}

	async u16(v: number) {
		await this.write(setUint16(this.#scratch, v));
	}

	async i32(v: number) {
		if (Math.abs(v) > MAX_U31) {
			throw new Error(`overflow, value larger than 32-bits: ${v.toString()}`);
		}

		// We don't use a varint, so it always takes 4 bytes.
		// This could be improved but nothing is standardized yet.
		await this.write(setInt32(this.#scratch, v));
	}

	async u53(v: number) {
		if (!Number.isSafeInteger(v) || v < 0) {
			throw new RangeError(`invalid u53: ${v}`);
		}
		await this.#varint(Math.floor(v / POW32), v >>> 0);
	}

	async u62(v: bigint) {
		const lo = split(v);
		await this.#varint(parts.hi, lo);
	}

	async varint(v: U64) {
		await this.#varint(v.hi, v.lo);
	}

	#varint(hi: number, lo: number): Promise<void> {
		return this.write(encodeTo(this.#scratch, hi, lo, this.version));
	}

	async write(v: Uint8Array) {
		// Mirrors Reader.#fill: every write funnels through here, so a STOP_SENDING from the
		// peer surfaces as a typed code rather than the transport's own error shape.
		await this.#writer.write(v).catch((err: unknown) => {
			throw fromTransport(err, { version: asIetf(this.version) });
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
			throw fromTransport(err, { version: asIetf(this.version) });
		});
		return this.#closed;
	}

	reset(reason: unknown) {
		this.#writer.abort(withCode(reason, this.version)).catch(() => void 0);
	}

	/**
	 * Open an outgoing unidirectional stream.
	 * @param quic - The session to open it on
	 * @param options - The version its varints encode with, and the send order ranking it
	 *   against the session's other streams
	 */
	static async open(quic: WebTransport, options: OpenOptions): Promise<Writer> {
		const writable = await openWithin(
			quic.createUnidirectionalStream(sendOptions(options)) as Promise<WritableStream<Uint8Array>>,
			options?.timeout ?? OPEN_TIMEOUT_MS,
			(stream) => void stream.abort().catch(() => void 0),
		);

		return new Writer(writable, options.version);
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
		// Raced ahead of the open, so an already-cancelled caller wins even against a slot
		// that is free right now. `race` rather than `Promise.race`: the caller shares one
		// `cancel` across every group of a subscription, which must not gain a reaction per call.
		const open = Writer.open(quic, options);

		// Resets a stream that opens after we gave up; a no-op if the open itself failed.
		const abandon = () => {
			const abandoned = new Error("abandoned waiting for a stream slot");
			open.then((w) => w.reset(abandoned)).catch(() => void 0);
		};

		try {
			const stream = await race([options.cancel, open]);
			if (stream) return stream;
		} catch (err: unknown) {
			// open already discarded the late stream on its way out.
			if (err instanceof TimeoutError) return undefined;
			// A rejected `cancel` still leaves the open pending.
			abandon();
			throw err;
		}

		abandon();
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
	#version: StreamVersion;

	constructor(quic: WebTransport, version: StreamVersion) {
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
