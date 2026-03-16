import type { IetfVersion } from "./ietf/version.ts";
import { Version } from "./ietf/version.ts";
import * as Varint from "./varint.ts";

const MAX_U31 = 2 ** 31 - 1;
const MAX_READ_SIZE = 1024 * 1024 * 64; // don't allocate more than 64MB for a message

function isLeadingOnes(version?: IetfVersion): boolean {
	return (
		version !== undefined &&
		version !== Version.DRAFT_14 &&
		version !== Version.DRAFT_15 &&
		version !== Version.DRAFT_16
	);
}

export class Stream {
	reader: Reader;
	writer: Writer;

	constructor(props: {
		writable: WritableStream<Uint8Array>;
		readable: ReadableStream<Uint8Array>;
		writer?: Writer;
		reader?: Reader;
		version?: IetfVersion;
	}) {
		this.writer = props.writer ?? new Writer(props.writable, props.version);
		this.reader = props.reader ?? new Reader(props.readable, undefined, props.version);
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

	static async open(quic: WebTransport, version?: IetfVersion, priority?: number): Promise<Stream> {
		const { readable, writable } = await quic.createBidirectionalStream({ sendOrder: priority });
		return new Stream({ readable, writable, version });
	}

	close() {
		this.writer.close();
		this.reader.stop(new Error("cancel"));
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

		const result = await this.#reader.read();
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

	// Returns a Number using 53-bits, the max Javascript can use for integer math
	async u53(): Promise<number> {
		const v = await this.u62();
		if (v > Varint.MAX_U53) {
			throw new Error("value larger than 53-bits; use v62 instead");
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

		if (ones === 6) throw new Error("invalid leading-ones varint: 1111110x prefix is reserved");

		let totalSize: number;
		if (ones <= 5) totalSize = ones + 1;
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
		this.#reader?.cancel(reason).catch(() => void 0);
	}

	get closed(): Promise<void> {
		return this.#reader?.closed ?? Promise.resolve();
	}
}

// Writer wraps a stream and writes chunks of data
export class Writer {
	#writer: WritableStreamDefaultWriter<Uint8Array>;
	#stream: WritableStream<Uint8Array>;

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
			throw new Error(`overflow, value larger than 53-bits: ${v.toString()}`);
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
		await this.#writer.write(v);
	}

	async string(str: string) {
		const data = new TextEncoder().encode(str);
		await this.u53(data.byteLength);
		await this.write(data);
	}

	close() {
		this.#writer.close().catch(() => void 0);
	}

	get closed(): Promise<void> {
		return this.#writer.closed;
	}

	reset(reason: unknown) {
		this.#writer.abort(reason).catch(() => void 0);
	}

	static async open(quic: WebTransport, version?: IetfVersion): Promise<Writer> {
		const writable = (await quic.createUnidirectionalStream()) as WritableStream<Uint8Array>;
		return new Writer(writable, version);
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
