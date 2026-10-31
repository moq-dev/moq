import { Group, Error as NetError } from "@moq/net";
import type * as z from "@zod/mini";

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
 * one leaves a gap in the log but nothing undecodable, and encoding continues; it is just not
 * charged to the group's budget.
 */
export interface Pending {
	/** The frame payload to write. */
	payload: Uint8Array;

	/**
	 * Acknowledge that the record reached the wire, charging it to the group's budget.
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
 * Payloads must reach the wire in order. If the caller does roll a group, call {@link reset} so the
 * budget restarts with it.
 */
export class Encoder<T> {
	#schema?: Config<T>["schema"];

	// Frames and bytes committed to the current group, checked against the group budget before each
	// record is encoded. Replaced on reset, so a commit that lands after one charges the old group
	// rather than the new.
	#budget = { frames: 0, bytes: 0 };

	constructor(config: Config<T> = {}) {
		this.#schema = config.schema;
	}

	/** Restart the group's budget, for a caller that has just rolled a group. */
	reset(): void {
		this.#budget = { frames: 0, bytes: 0 };
	}

	/**
	 * Encode one record into the next frame payload.
	 *
	 * The record comes back as a {@link Pending} the caller writes and then commits.
	 *
	 * Throws `GroupTooLarge` if the record does not fit in what is left of the group's budget
	 * (`MAX_GROUP_CACHE_BYTES` and `MAX_GROUP_FRAMES`), counting every record committed since the
	 * last {@link reset}. The refused record leaves the encoder untouched.
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
		const budget = this.#budget;
		if (
			budget.frames >= Group.MAX_GROUP_FRAMES ||
			budget.bytes + payload.byteLength > Group.MAX_GROUP_CACHE_BYTES
		) {
			throw new NetError.GroupTooLarge();
		}

		let charged = false;
		return {
			payload,
			commit: () => {
				// The record landed once, however often it is acknowledged.
				if (charged) return;
				charged = true;
				budget.frames++;
				budget.bytes += payload.byteLength;
			},
		};
	}
}
