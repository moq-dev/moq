import { Encoder as Flate } from "@moq/flate";
import type * as z from "zod/mini";

import { deepEqual, diff } from "../diff.ts";

// Maximum frames (snapshot + deltas) in a single group before a new snapshot is forced. Kept
// well below the per-group frame cap so a late joiner can always read the snapshot at frame 0.
const MAX_DELTA_FRAMES = 256;

// Delta ratio used when {@link Config.deltaRatio} is left unset. Not re-exported from the package
// entrypoint: the Producer needs it to know whether to hold a group open, nothing else does.
export const DEFAULT_DELTA_RATIO = 8;

/** Options shared by an {@link Encoder} and the {@link Producer} that wraps one. */
export interface Config<T> {
	// Controls how aggressively the encoder emits deltas (merge patches) instead of full snapshots.
	//
	// `0` disables deltas: every change is encoded as a new snapshot.
	//
	// A positive number enables deltas: a new snapshot is emitted once the deltas already written
	// to the current group (excluding the snapshot frame) exceed `deltaRatio` times the snapshot size.
	// The pending delta is excluded from that check, so the one that first crosses the budget still
	// lands before the group rolls. So `1` allows roughly one snapshot's worth of deltas before rolling.
	//
	// When {@link compression} is on, both sides of the comparison are measured on the compressed frame
	// sizes (the real wire cost).
	//
	// Defaults to `8` when unset.
	deltaRatio?: number;

	// Optional zod schema used to validate each value before publishing.
	schema?: z.ZodMiniType<T>;

	// Starting value for {@link Producer.mutate} before anything has been published. Required to
	// mutate a producer that hasn't published yet (e.g. a fresh catalog); ignored once a value exists.
	initial?: T;

	// Compress each group as one sync-flushed `deflate-raw` (RFC 1951) stream, so deltas reuse the
	// snapshot as context and shrink sharply. Interoperable with the Rust `moq-json` producer.
	// `false`/unset (the default) writes plaintext JSON frames. A {@link Decoder} reading them
	// must set the same flag.
	compression?: boolean;
}

/** One encoded frame, and the group boundary it implies. */
export interface Encoded {
	/** The frame payload, `deflate-raw` compressed when {@link Config.compression} is set. */
	payload: Uint8Array;

	/**
	 * Whether this frame is a full snapshot, which must open a new group.
	 *
	 * `true` means the caller writes it as the first frame of a fresh group; `false` means it is a
	 * merge patch that must be appended to the group the last snapshot opened.
	 *
	 * The encoder decides this, never the caller: a value that sets a field to JSON null, or whose
	 * root isn't an object, cannot be expressed as a merge patch at all, and the delta budget and
	 * frame cap force a snapshot independently of what the caller wanted.
	 */
	keyframe: boolean;
}

/**
 * Encodes a JSON value into frame payloads, choosing snapshots and deltas automatically.
 *
 * The track-free core of {@link Producer}: it decides *what bytes go in a frame* and *where the
 * group boundaries fall*, and leaves writing them to the caller. Reach for it when something else
 * already owns the track.
 *
 * Frames must reach the wire in the order they were encoded, and a frame with
 * {@link Encoded.keyframe} set must open a new group: both the merge patches and the group-scoped
 * DEFLATE window depend on it. If the caller closes a group for its own reasons, call
 * {@link reset} so the next value is encoded as a snapshot.
 */
export class Encoder<T> {
	#config: Config<T>;
	#compress: boolean;

	// The last encoded value, normalized through JSON so it matches what landed on the wire. The
	// baseline every delta is diffed against, and `undefined` until the first snapshot.
	#last?: unknown;

	// Bytes of deltas already emitted into the current group, excluding the snapshot frame. Compressed
	// frame sizes when compressing, raw otherwise, matching {@link #snapshotLen} so the budget check is
	// like-for-like (and identical to the Rust encoder).
	#deltaBytes = 0;
	// Size of the current group's snapshot frame, the reference the delta budget is measured against.
	#snapshotLen = 0;
	// Frames emitted into the current group, snapshot included.
	#groupFrames = 0;

	// The current group's `deflate-raw` stream, swapped for a fresh one (cold window) at each
	// snapshot, so a snapshot and its deltas share one stream.
	#flate?: Flate;

	constructor(config: Config<T> = {}) {
		this.#config = config;
		this.#compress = config.compression ?? false;
	}

	/** The last encoded value, or `undefined` before the first snapshot. */
	get value(): T | undefined {
		return this.#last as T | undefined;
	}

	/**
	 * Forget the baseline, so the next {@link update} emits a snapshot.
	 *
	 * Call this whenever the caller closes the current group behind the encoder's back. Without it
	 * the next value may be encoded as a delta against a DEFLATE window and a baseline that the new
	 * group doesn't carry.
	 */
	reset(): void {
		this.#last = undefined;
		this.#flate = undefined;
		this.#deltaBytes = 0;
		this.#snapshotLen = 0;
		this.#groupFrames = 0;
	}

	/**
	 * Encode a new value, as a snapshot or a delta.
	 *
	 * Returns `undefined` when the value is unchanged from the last one encoded, so nothing needs to
	 * be written.
	 */
	update(value: T): Encoded | undefined {
		const valid = this.#config.schema ? this.#config.schema.parse(value) : value;

		// Serialize once; parse it back to a normalized JSON value for diffing and comparison
		// (dropping `undefined` fields, matching what lands on the wire).
		const text = JSON.stringify(valid);
		const json = JSON.parse(text);
		if (this.#last !== undefined && deepEqual(this.#last, json)) return undefined;

		const delta = this.#delta(json);
		this.#last = json;

		if (delta) {
			const payload = this.#frame(delta);
			this.#deltaBytes += payload.length;
			this.#groupFrames += 1;
			return { payload, keyframe: false };
		}

		return { payload: this.#snapshot(new TextEncoder().encode(text)), keyframe: true };
	}

	// Resolved delta ratio: the configured value, or the default when unset. `0` disables deltas.
	get #deltaRatio(): number {
		return this.#config.deltaRatio ?? DEFAULT_DELTA_RATIO;
	}

	// Build a delta frame, or `undefined` to signal that a fresh snapshot should be emitted.
	//
	// The budget gate runs first, against the deltas already written, so rolling a new group costs no
	// merge-patch work. Since the gate excludes the frame about to be written, the delta that tips the
	// group past `ratio * snapshot` still lands: a group overshoots the budget by at most one delta.
	#delta(json: unknown): Uint8Array | undefined {
		const ratio = this.#deltaRatio;
		if (ratio === 0) return undefined;
		if (this.#last === undefined) return undefined;
		if (this.#groupFrames === 0 || this.#groupFrames >= MAX_DELTA_FRAMES) return undefined;

		// Gate on the deltas accumulated so far (snapshot frame excluded), before computing the patch.
		if (this.#deltaBytes > ratio * this.#snapshotLen) return undefined;

		const result = diff(this.#last, json);
		if (result.forcedSnapshot) return undefined;

		return new TextEncoder().encode(JSON.stringify(result.patch));
	}

	// Encode a group's snapshot (frame 0), returning its payload. On the compressed path this opens a
	// fresh stream (cold window), so the snapshot and its deltas share one DEFLATE window.
	#snapshot(frame: Uint8Array): Uint8Array {
		this.#flate = this.#compress ? new Flate() : undefined;

		const payload = this.#frame(frame);
		this.#snapshotLen = payload.length;
		this.#deltaBytes = 0;
		this.#groupFrames = 1;

		return payload;
	}

	// Compress a frame into the current group's window, or pass it through when uncompressed.
	#frame(frame: Uint8Array): Uint8Array {
		return this.#flate ? this.#flate.frame(frame) : frame;
	}
}
