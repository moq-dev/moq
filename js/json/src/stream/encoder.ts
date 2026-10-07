import { Encoder as Flate } from "@moq/flate";
import { Group, Error as NetError } from "@moq/net";
import type * as z from "@zod/mini";

import { type Compression, isDeflate } from "../compression.ts";
import { Desync } from "../error.ts";

/** Options for an {@link Encoder}. */
export interface Config<T = unknown> {
	/** Validate each record before publishing and after decoding. */
	schema?: z.ZodMiniType<T>;
	/**
	 * Compress the group as one sync-flushed `deflate-raw` stream, so each record reuses the earlier
	 * ones as context and shrinks sharply. A {@link Decoder} reading the frames must set the same
	 * {@link compression}. Defaults to `"none"`.
	 */
	compression?: Compression;
}

/**
 * An encoded record the caller has not yet acknowledged writing, returned by {@link Encoder.encode}.
 *
 * Write the {@link payload}, then {@link commit}.
 *
 * A record that is never committed never reached the wire. With compression on that is
 * unrecoverable within the group: the window is ahead of what the consumer holds, and a log has no
 * keyframe to resynchronize on the way `Snapshot` does. So the encoder throws on the next
 * {@link Encoder.encode} until the caller rolls a new group and calls {@link Encoder.reset}. Without
 * compression each record stands alone, so a dropped one leaves a gap in the log but nothing
 * undecodable, and encoding continues.
 */
export interface Pending {
	/** The frame payload to write. */
	payload: Uint8Array;

	/**
	 * Acknowledge that the record reached the wire, keeping the encoder's window.
	 *
	 * Only call this once the write has actually succeeded.
	 */
	commit(): void;
}

/**
 * Encodes JSON records into frame payloads, sharing one DEFLATE window across the log.
 *
 * The track-free core of {@link Producer}. Unlike the `Snapshot` encoder there are no group
 * boundaries to report: a log is an unbroken sequence of self-contained records, so every payload is
 * simply the next frame.
 *
 * The window spans everything encoded so far, so payloads must reach the wire in order and be
 * decoded in the same order. If the caller does roll a group, call {@link reset} so the next record
 * starts a cold window that the new group's decoder can follow.
 */
export class Encoder<T> {
	#schema?: Config<T>["schema"];
	#compress: boolean;
	// The DEFLATE window for the whole log, present while compressing.
	#flate?: Flate;

	// Set when a compressed record was encoded but never written. The window is then ahead of the
	// consumer for the rest of the group, so encoding stops until the caller rolls a new one.
	#desynced = false;
	// Whether the record from the last {@link encode} is still unacknowledged.
	#pending = false;
	// Bumped for each record handed out, so a commit that arrives after the encoder has moved on can
	// tell that it is acknowledging a record that is no longer the outstanding one.
	#generation = 0;
	// Frames and payload bytes committed to the current group, checked against the group budget
	// before each record is encoded. Replaced on reset, so a commit that lands after one charges the
	// old group rather than the new.
	#budget = { frames: 0, bytes: 0 };

	constructor(config: Config<T> = {}) {
		this.#schema = config.schema;
		this.#compress = isDeflate(config.compression);
		this.#flate = this.#compress ? new Flate() : undefined;
	}

	/**
	 * Start a cold DEFLATE window, for a caller that has just rolled a group.
	 *
	 * This is also how a caller clears a desync: roll a new group so the consumer starts its own cold
	 * window, then reset.
	 */
	reset(): void {
		this.#flate = this.#compress ? new Flate() : undefined;
		this.#desynced = false;
		this.#pending = false;
		this.#budget = { frames: 0, bytes: 0 };
	}

	/**
	 * Encode one record into the next frame payload.
	 *
	 * The record comes back as a {@link Pending} the caller writes and then commits. Throws if a
	 * previous compressed record was left uncommitted, since every frame after it would be
	 * undecodable.
	 *
	 * Throws `GroupTooLarge` if the record might not fit in what is left of the group's budget
	 * (`MAX_GROUP_CACHE_BYTES` and `MAX_GROUP_FRAMES`), counting every record committed since the
	 * last {@link reset}. The refused record leaves the encoder untouched.
	 */
	encode(value: T): Pending {
		// An uncompressed record carries no shared state, so losing one leaves a gap in the log rather
		// than an undecodable stream, and encoding continues.
		if (this.#pending && this.#compress) this.#desynced = true;
		if (this.#desynced) {
			throw new Desync();
		}

		const valid = this.#schema ? this.#schema.parse(value) : value;
		const text = JSON.stringify(valid);
		if (text === undefined) {
			// `JSON.stringify` yields undefined for a top-level undefined, function, or symbol, which
			// would otherwise frame as empty bytes and fail on the consumer instead of here.
			throw new Error("record is not representable as JSON");
		}

		const bytes = new TextEncoder().encode(text);

		// Check before compressing: encoding advances the window, so a record refused afterwards
		// would leave the encoder ahead of every reader. The worst case is checked rather than the
		// actual size for the same reason. The budget is also below `@moq/flate`'s per-frame decode
		// cap, so any record that fits is one every consumer can inflate.
		const budget = this.#budget;
		const bound = this.#compress ? deflateBound(bytes.byteLength) : bytes.byteLength;
		if (budget.frames >= Group.MAX_GROUP_FRAMES || budget.bytes + bound > Group.MAX_GROUP_CACHE_BYTES) {
			throw new NetError.GroupTooLarge();
		}

		const payload = this.#flate ? this.#flate.frame(bytes) : bytes;

		this.#pending = true;
		const generation = ++this.#generation;
		let charged = false;

		return {
			payload,
			commit: () => {
				// The record landed once, however often it is acknowledged.
				if (!charged) {
					charged = true;
					budget.frames++;
					budget.bytes += payload.byteLength;
				}
				// A caller that starts the next encode before this record settles has already made the
				// encoder account for it. Acknowledging it now would clear the flag belonging to the newer
				// record, so a later loss of that one would go unnoticed.
				if (this.#generation === generation) this.#pending = false;
			},
		};
	}
}

// The largest a sync-flushed DEFLATE frame of `len` raw bytes can grow to: zlib's `deflateBound` for
// its default window and memory level, which both pako and moq-flate use. Incompressible input falls
// back to stored blocks, 5 bytes per 16 KiB; the constant covers the block headers and the flush,
// whose fixed 4-byte marker is stripped anyway. Division rather than shifts, which wrap past 2^31.
export function deflateBound(len: number): number {
	return len + Math.floor(len / 4096) + Math.floor(len / 16384) + Math.floor(len / 33554432) + 13;
}
