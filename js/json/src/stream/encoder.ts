import { Error as NetError } from "@moq/net";
import type * as z from "@zod/mini";
import { MAX_FRAME_BYTES } from "../limits.ts";

/** Options for an {@link Encoder}. */
export interface Config<T = unknown> {
	/** Validate each record before publishing and after decoding. */
	schema?: z.ZodMiniType<T>;
}

/**
 * An encoded record the caller has not yet acknowledged writing, returned by {@link Encoder.encode}.
 *
 * Write the {@link payload}, then {@link commit}.
 *
 * A record that is never committed never reached the wire. Each record stands alone, so a dropped
 * one leaves a gap in the log but nothing undecodable, and encoding continues.
 */
export interface Pending {
	/** The frame payload to write. */
	payload: Uint8Array;

	/**
	 * Acknowledge that the record reached the wire.
	 *
	 * Only call this once the write has actually succeeded.
	 */
	commit(): void;
}

/**
 * Encodes JSON records into frame payloads.
 *
 * The track-free core of {@link Producer}. Unlike the `Snapshot` encoder there are no group
 * boundaries to report: a log is an unbroken sequence of self-contained records, so every payload is
 * simply the next frame.
 *
 * Payloads must reach the wire in order. A group has no size cap, so the encoder keeps no budget.
 */
export class Encoder<T> {
	#schema?: Config<T>["schema"];

	constructor(config: Config<T> = {}) {
		this.#schema = config.schema;
	}

	/**
	 * Encode one record into the next frame payload.
	 *
	 * The record comes back as a {@link Pending} the caller writes and then commits.
	 * A record larger than the whole-frame limit throws `FrameTooLarge`.
	 */
	encode(value: T): Pending {
		const valid = this.#schema ? this.#schema.parse(value) : value;
		const text = JSON.stringify(valid);
		if (text === undefined) {
			// `JSON.stringify` yields undefined for a top-level undefined, function, or symbol, which
			// would otherwise frame as empty bytes and fail on the consumer instead of here.
			throw new Error("record is not representable as JSON");
		}

		const payload = new TextEncoder().encode(text);
		if (payload.length > MAX_FRAME_BYTES) throw new NetError.FrameTooLarge();
		return { payload, commit: () => {} };
	}
}
