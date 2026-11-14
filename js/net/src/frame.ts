/**
 * Frame role handles: one frame's payload written or read in pieces as it arrives.
 *
 * @module
 */
import type { Instant } from "@moq/clock";
import { type Getter, Once, race, Signal } from "@moq/signals";
import { StreamCode, StreamError } from "./error.ts";
import { stored } from "./flate/group.ts";
import type { Frame } from "./group.ts";
import { hooks } from "./internal.ts";
import { Queue } from "./queue.ts";
import type { Timestamp } from "./time.ts";
import { SEGMENT_BYTES } from "./util/limits.ts";

/**
 * A frame's header, declared before its payload, as Rust's `frame::Info`.
 *
 * @public
 */
export interface Info {
	/** The payload size in bytes, which the writes must add up to exactly. */
	size: number;
	/**
	 * Presentation timestamp, or on a scale-less track {@link Timestamp.unscaled} at its group's
	 * key (see `Group.Frame.timestamp`).
	 */
	timestamp: Timestamp;
}

// A frame written in pieces, shared by its group and every mirror. Written whole, it stays the
// writer's one buffer; otherwise it is one buffer within a segment, or a chain of
// `SEGMENT_BYTES` segments, each allocated by its first byte. Sealed segments shed from the
// front under the group's tail budget, so a reader that falls behind inside it is `Evicted`.
/** @internal */
export class Chunked implements Frame {
	readonly size: number;
	readonly timestamp: Timestamp;
	arrived?: Instant;
	// Bytes written so far, which a streaming reader waits on.
	readonly written = new Signal(0);
	// Settled once every byte is written (`null`), or with the failure that cut the frame short.
	readonly done = new Once<Error | null>();
	// The frame offset of the first retained segment.
	base = 0;
	// Allocated bytes across the retained segments.
	retained = 0;
	#segments = new Queue<Uint8Array>();
	// Whether one write carried the whole frame, which is then a single buffer of any size.
	#whole = false;

	constructor(info: Info) {
		this.size = info.size;
		this.timestamp = info.timestamp;
	}

	// Append `chunk`, returning the bytes newly allocated for it.
	write(chunk: Uint8Array): number {
		let at = this.written.peek();
		if (at === 0 && chunk.byteLength === this.size) {
			this.#whole = true;
			this.#segments.push(chunk);
			this.retained += chunk.byteLength;
			this.written.set(this.size);
			return chunk.byteLength;
		}
		let allocated = 0;
		let offset = 0;
		while (offset < chunk.byteLength) {
			// The open segment ends at the next segment boundary, or the frame's end.
			if (at % SEGMENT_BYTES === 0) {
				const segment = new Uint8Array(Math.min(SEGMENT_BYTES, this.size - at));
				this.#segments.push(segment);
				this.retained += segment.byteLength;
				allocated += segment.byteLength;
			}
			const segment = this.#segments.last as Uint8Array;
			const start = at % SEGMENT_BYTES;
			const n = Math.min(chunk.byteLength - offset, segment.byteLength - start);
			segment.set(chunk.subarray(offset, offset + n), start);
			offset += n;
			at += n;
		}
		this.written.set(at);
		return allocated;
	}

	// The written bytes from frame offset `at` to the end of its segment, as a view.
	chunk(at: number): Uint8Array {
		const written = this.written.peek();
		if (this.#whole) {
			const whole = this.#segments.first as Uint8Array;
			return at === 0 ? whole : whole.subarray(at, written);
		}
		const index = Math.floor(at / SEGMENT_BYTES);
		const segment = this.#segments.at(index - this.base / SEGMENT_BYTES) as Uint8Array;
		const start = index * SEGMENT_BYTES;
		return segment.subarray(at - start, Math.min(segment.byteLength, written - start));
	}

	// Drop the oldest sealed segment, returning its bytes, or 0 when only the newest is left.
	shed(): number {
		if (this.#segments.length <= 1) return 0;
		const segment = this.#segments.shift() as Uint8Array;
		this.base += segment.byteLength;
		this.retained -= segment.byteLength;
		return segment.byteLength;
	}

	// Release every segment once the group lets go of the frame.
	release(): void {
		this.#segments = new Queue();
		this.base = this.size;
		this.retained = 0;
	}

	get payload(): Uint8Array {
		const done = this.done.peek();
		if (done instanceof Error) throw done;
		if (done === undefined) throw new Error("frame is still being written");
		if (this.base > 0) throw evicted();
		if (this.#segments.length === 1) return this.chunk(0);
		const out = new Uint8Array(this.size);
		let offset = 0;
		for (const segment of this.#segments) {
			out.set(segment, offset);
			offset += segment.byteLength;
		}
		return out;
	}
}

// The payload size, without joining a chunked frame's segments.
/** @internal */
export function payloadBytes(frame: Frame): number {
	if (frame instanceof Chunked) return frame.size;
	return stored(frame)?.payload.length ?? frame.payload.byteLength;
}

/** @internal */
export const EMPTY = new Uint8Array(0);

/** @internal */
export function evicted(): Error {
	return new StreamError(StreamCode.Evicted, { message: "group front was evicted" });
}

// What a frame producer drives in its group.
/** @internal */
export interface Ops {
	written(): number;
	write(chunk: Uint8Array): void;
	finish(): void;
	abort(error: Error): void;
}

/** @internal */
export let makeProducer: (info: Info, ops: Ops) => Producer;

/**
 * Writes one frame's payload in pieces, as Rust's `frame::Producer`. Obtain one from
 * `Group.Producer.createFrame`.
 *
 * The writes must add up to the declared size exactly, and {@link finish} commits the frame.
 * Its group takes no other frame until then; ending the group first fails the frame.
 *
 * @public
 */
export class Producer {
	/** The declared payload size in bytes. */
	readonly size: number;
	/** The frame's presentation timestamp, or on a scale-less track its group's key. */
	readonly timestamp: Timestamp;
	#ops: Ops;
	#done = false;

	private constructor(info: Info, ops: Ops) {
		this.size = info.size;
		this.timestamp = info.timestamp;
		this.#ops = ops;
	}

	static {
		makeProducer = (info, ops) => new Producer(info, ops);
	}

	/** Bytes still needed to complete the frame. */
	get remaining(): number {
		return this.size - this.#ops.written();
	}

	/** Append `chunk`, waking readers streaming the frame. The bytes are copied unless one write carries the whole frame. */
	write(chunk: Uint8Array): void {
		if (this.#done) throw new Error("frame is closed");
		if (chunk.byteLength > this.remaining) throw new RangeError("write exceeds the frame's declared size");
		if (chunk.byteLength > 0) this.#ops.write(chunk);
	}

	/** Commit the frame once every declared byte is written. */
	finish(): void {
		if (this.#done) throw new Error("frame is closed");
		if (this.remaining !== 0) throw new RangeError(`frame is ${this.remaining} bytes short of its declared size`);
		this.#done = true;
		this.#ops.finish();
	}

	/** Fail the frame and its group with `error`. A no-op once the frame finished or aborted. */
	abort(error: Error): void {
		if (this.#done) return;
		this.#done = true;
		this.#ops.abort(error);
	}
}

/** @internal */
export let makeConsumer: (sequence: number, frame: Frame) => Consumer;
// The next chunk without waiting: `undefined` at the end, or `WAIT` until the next write.
/** @internal */
export let pollChunk: (consumer: Consumer) => Uint8Array | undefined | typeof WAIT;
// What a waiting chunk read waits on.
/** @internal */
export let chunkChanged: (consumer: Consumer) => Getter<unknown>[];
/** @internal */
export const WAIT: unique symbol = Symbol("wait");

// Parts read in order, joined into one buffer, copied only when there is more than one.
/** @internal */
export function joined(parts: Uint8Array[], size: number): Uint8Array {
	if (parts.length === 1) return parts[0];
	const out = new Uint8Array(size);
	let at = 0;
	for (const part of parts) {
		out.set(part, at);
		at += part.byteLength;
	}
	return out;
}

/**
 * Reads one frame's payload as it arrives, as Rust's `frame::Consumer`. Obtain one from
 * `Group.Consumer.nextFrame`.
 *
 * Chunks are read-only views shared with other readers. A frame larger than its group's
 * retained tail sheds its oldest segments while it is written, so a reader that falls that
 * far behind fails with a `StreamError` of code `Evicted`.
 *
 * @public
 */
export class Consumer {
	/** The frame's sequence number within its group. */
	readonly sequence: number;
	/** The payload size in bytes. */
	readonly size: number;
	/** The frame's presentation timestamp, or on a scale-less track its group's key. */
	readonly timestamp: Timestamp;
	#frame: Frame;
	#offset = 0;

	private constructor(sequence: number, frame: Frame) {
		this.sequence = sequence;
		this.#frame = frame;
		this.size = payloadBytes(frame);
		this.timestamp = frame.timestamp;
	}

	static {
		makeConsumer = (sequence, frame) => new Consumer(sequence, frame);
		hooks.readFrameChunk = (consumer, closed) => consumer.#readChunk(closed);
		pollChunk = (consumer) => consumer.#poll();
		chunkChanged = (consumer) => {
			const frame = consumer.#frame;
			return frame instanceof Chunked ? [frame.written, frame.done] : [];
		};
	}

	/** When the frame's last byte landed in this node's group; `undefined` until then. */
	get arrived(): Instant | undefined {
		return this.#frame.arrived;
	}

	#poll(): Uint8Array | undefined | typeof WAIT {
		const frame = this.#frame;
		if (!(frame instanceof Chunked)) {
			if (this.#offset >= this.size) return undefined;
			this.#offset = this.size;
			return frame.payload;
		}
		const done = frame.done.peek();
		if (done instanceof Error) throw done;
		if (this.#offset < frame.base) throw evicted();
		if (this.#offset < frame.written.peek()) {
			const chunk = frame.chunk(this.#offset);
			this.#offset += chunk.byteLength;
			return chunk;
		}
		return done === null ? undefined : WAIT;
	}

	/** The next chunk of the payload, waiting for it to be written; `undefined` once every byte is read. */
	readChunk(): Promise<Uint8Array | undefined> {
		return this.#readChunk();
	}

	async #readChunk(closed?: Promise<void>): Promise<Uint8Array | undefined> {
		for (;;) {
			const chunk = this.#poll();
			if (chunk !== WAIT) return chunk;
			const changed = Signal.race(...chunkChanged(this));
			if (closed === undefined) await changed;
			// A frame changes with a byte count or a verdict; only the stream resolves void.
			else if ((await race([changed, closed])) === undefined) return undefined;
		}
	}

	/**
	 * The rest of the payload in one buffer, copied only when it spans chunks. It grows as
	 * bytes arrive, so a declared size is never allocated ahead of them.
	 */
	async readAll(): Promise<Uint8Array> {
		const parts: Uint8Array[] = [];
		let size = 0;
		for (let chunk = await this.readChunk(); chunk; chunk = await this.readChunk()) {
			parts.push(chunk);
			size += chunk.byteLength;
		}
		return parts.length === 0 ? EMPTY : joined(parts, size);
	}
}
