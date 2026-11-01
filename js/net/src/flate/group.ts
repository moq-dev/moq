import type { Instant } from "@moq/clock";
import { StreamCode, StreamError } from "../error.ts";
import type { Frame } from "../group.ts";
import { Queue } from "../queue.ts";
import type { Timestamp } from "../time.ts";
import { MAX_FRAME_BYTES } from "../util/limits.ts";
import { Decoder } from "./decoder.ts";
import { Encoder } from "./encoder.ts";

/** A stored slice and its declared plain size. Package-private wire metadata. */
export interface Stored {
	/** The original compressed bytes. */
	payload: Uint8Array;
	/** The exact plain payload length. */
	inflated: number;
}

// Class identity keeps reactive equality from reading the lazy payload getter.
class SliceFrame implements Frame {
	readonly timestamp: Timestamp | undefined;
	readonly arrived: Instant | undefined;
	#read: () => Uint8Array;
	constructor(timestamp: Timestamp | undefined, arrived: Instant | undefined, read: () => Uint8Array) {
		this.timestamp = timestamp;
		this.arrived = arrived;
		this.#read = read;
	}
	get payload(): Uint8Array {
		return this.#read();
	}
}

const slices = new WeakMap<Frame, Stored>();

/**
 * Whether a local plain payload of `size` bytes encodes to a slice a peer accepts, whatever it
 * holds: checked before encoding, since an encoded frame has already advanced the dictionary.
 */
export function encodable(size: number): boolean {
	return Encoder.bound(size) <= MAX_FRAME_BYTES;
}

/** Read a compressed frame's stored form without inflating it. */
export function stored(frame: Frame): Stored | undefined {
	return slices.get(frame);
}

/**
 * A datagram's stored form: a local payload encoded, or a received slice checked to inflate
 * to `inflated` bytes. The checked copy is dropped, since each reader inflates its own.
 */
export function datagramSlice(payload: Uint8Array, inflated?: number): Stored {
	if (inflated === undefined) return { payload: new Encoder().frame(payload), inflated: payload.length };
	new Decoder().frame(payload, inflated);
	return { payload, inflated };
}

/** One reader's lazy plain view of a slice, so no queue retains a copy another reader inflated. */
export function sliceView(timestamp: Timestamp | undefined, slice: Stored): Frame {
	let plain: Uint8Array | undefined;
	const frame = new SliceFrame(timestamp, undefined, () => {
		plain ??= new Decoder().frame(slice.payload, slice.inflated);
		return plain;
	});
	slices.set(frame, slice);
	return frame;
}

interface Entry {
	index: number;
	slice: Stored;
	plain?: Uint8Array;
}

/** One group's stored tail and lazy plain copy, shared by every mirror. */
export class Compression {
	#encoder?: Encoder;
	#decoder?: Decoder;
	#frames = new Queue<Entry>();
	#pending = new Queue<Entry>();
	#next = 0;
	#decoded = 0;
	#base = 0;
	#failed?: Error;
	#finished = false;
	/** Retained compressed payload bytes, separate from its optional plain copy. */
	stored = 0;
	/** Retained slice and shared plain bytes. */
	bytes = 0;
	/** Called once a slice fails to decode, so the owning group fails with it. */
	failed?: (error: Error) => void;

	/** Store a local plain frame or a received slice. */
	push(frame: Frame, inflated?: number): Frame {
		// A failed dictionary cannot decode anything after it, so the group takes no more.
		if (this.#failed) throw this.#failed;
		let slice: Stored;
		if (inflated === undefined) {
			this.#encoder ??= new Encoder();
			slice = { payload: this.#encoder.frame(frame.payload), inflated: frame.payload.length };
		} else slice = { payload: frame.payload, inflated };
		this.stored += slice.payload.length;
		this.bytes += slice.payload.length;
		const entry: Entry = { index: this.#next++, slice };
		this.#frames.push(entry);
		this.#pending.push(entry);
		const result = new SliceFrame(frame.timestamp, frame.arrived, () => this.#inflate(entry));
		slices.set(result, slice);
		return result;
	}

	#inflate(entry: Entry): Uint8Array {
		if (this.#failed) throw this.#failed;
		if (entry.index < this.#base) throw new StreamError(StreamCode.Evicted);
		if (entry.plain) return entry.plain;
		try {
			while (this.#pending.first && this.#pending.first.index <= entry.index) this.#decode(this.#pending.first);
			if (this.#finished && this.#decoded === this.#next) this.#decoder = undefined;
			if (!entry.plain) throw new Error("compressed frame is missing");
			return entry.plain;
		} catch (error) {
			this.abort(error instanceof Error ? error : new Error(String(error)));
			throw this.#failed;
		}
	}

	#decode(entry: Entry): void {
		this.#decoder ??= new Decoder();
		entry.plain = this.#decoder.frame(entry.slice.payload, entry.slice.inflated);
		this.#pending.shift();
		this.#decoded = entry.index + 1;
		this.bytes += entry.plain.length;
	}

	/** Advance the dictionary through a shed prefix, retaining neither its slices nor plain bytes. */
	shed(base: number): void {
		while (this.#frames.first && this.#frames.first.index < base) {
			const entry = this.#frames.first;
			// Later slices depend on this dictionary even if no plain reader took the prefix.
			if (entry.index >= this.#decoded && !this.#failed) {
				try {
					this.#decode(entry);
				} catch (error) {
					this.abort(error instanceof Error ? error : new Error(String(error)));
					break;
				}
			}
			this.#frames.shift();
			this.stored -= entry.slice.payload.length;
			this.bytes -= entry.slice.payload.length + (entry.plain?.length ?? 0);
			entry.plain = undefined;
		}
		this.#base = base;
	}

	/** Release cached content on an abort, failing every mirror's late read. */
	abort(error: Error): void {
		this.#failed = error;
		this.#encoder = undefined;
		this.#decoder = undefined;
		for (const entry of this.#frames) entry.plain = undefined;
		this.#frames = new Queue();
		this.#pending = new Queue();
		this.bytes = 0;
		this.stored = 0;
		this.failed?.(error);
	}

	/** Release codec scratch once no more frames can arrive. */
	finish(): void {
		this.#finished = true;
		this.#encoder = undefined;
		if (this.#decoded === this.#next) this.#decoder = undefined;
	}
}
