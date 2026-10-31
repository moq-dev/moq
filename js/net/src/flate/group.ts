import type { Instant } from "@moq/clock";
import type { Frame } from "../group.ts";
import type { Timestamp } from "../time.ts";
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

/** One group's stored form and lazy plain copy, shared by every mirror. */
export class Compression {
	#encoder?: Encoder;
	#decoder?: Decoder;
	#frames: Stored[] = [];
	#plain: Uint8Array[] = [];
	#failed?: Error;
	#finished = false;
	/** Plain bytes admitted against the group budget. */
	declared = 0;
	/** Retained compressed payload bytes, separate from its optional plain copy. */
	stored = 0;
	/** Retained slice and shared plain bytes. */
	bytes = 0;

	/** Store a local plain frame or a received slice after budget admission. */
	push(frame: Frame, inflated?: number): Frame {
		let slice: Stored;
		if (inflated === undefined) {
			this.#encoder ??= new Encoder();
			slice = { payload: this.#encoder.frame(frame.payload), inflated: frame.payload.length };
		} else slice = { payload: frame.payload, inflated };
		this.declared += slice.inflated;
		this.stored += slice.payload.length;
		this.bytes += slice.payload.length;
		const index = this.#frames.length;
		this.#frames.push(slice);
		const result = new SliceFrame(frame.timestamp, frame.arrived, () => this.#inflate(index));
		slices.set(result, slice);
		return result;
	}

	#inflate(index: number): Uint8Array {
		if (this.#failed) throw this.#failed;
		try {
			while (this.#plain.length <= index) {
				const slice = this.#frames[this.#plain.length];
				this.#decoder ??= new Decoder();
				const plain = this.#decoder.frame(slice.payload, slice.inflated);
				this.#plain.push(plain);
				this.bytes += plain.length;
			}
			if (this.#finished && this.#plain.length === this.#frames.length) this.#decoder = undefined;
			return this.#plain[index];
		} catch (error) {
			this.#failed = error instanceof Error ? error : new Error(String(error));
			this.#decoder = undefined;
			throw this.#failed;
		}
	}

	/** Release cached content on an abort, failing every mirror's late read. */
	abort(error: Error): void {
		this.#failed = error;
		this.#encoder = undefined;
		this.#decoder = undefined;
		this.#frames = [];
		this.#plain = [];
		this.bytes = 0;
		this.stored = 0;
	}

	/** Release codec scratch once no more frames can arrive. */
	finish(): void {
		this.#finished = true;
		this.#encoder = undefined;
		if (this.#plain.length === this.#frames.length) this.#decoder = undefined;
	}
}
