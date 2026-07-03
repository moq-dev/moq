import { Encoder } from "@moq/flate";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import type * as z from "zod/mini";

import { deepEqual, diff } from "./diff.ts";

// Maximum frames (snapshot + deltas) in a single group before a new snapshot is forced. Kept
// well below the per-group frame cap so a late joiner can always read the snapshot at frame 0.
const MAX_DELTA_FRAMES = 256;

// Delta ratio used when {@link Config.deltaRatio} is left unset.
const DEFAULT_DELTA_RATIO = 8;

export interface Config<T> {
	// Controls how aggressively the producer emits deltas (merge patches) instead of full snapshots.
	//
	// `0` disables deltas: every change is published as a new snapshot group.
	//
	// A positive number enables deltas: a new snapshot group is started once the deltas already written
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
	// `false`/unset (the default) writes plaintext JSON frames. A {@link Consumer} reading the track
	// must set the same flag.
	compression?: boolean;
}

/** Publishes a JSON value over a track, choosing snapshots and deltas automatically. */
export class Producer<T> {
	#track: Moq.TrackProducer;
	#config: Config<T>;

	#group?: Moq.Group;
	#last?: unknown;
	// Bytes of deltas already written to the current group, excluding the snapshot frame. Compressed
	// frame sizes when compressing, raw otherwise, matching {@link #snapshotLen} so the budget check is
	// like-for-like (and identical to the Rust producer).
	#deltaBytes = 0;
	// Size of the current group's snapshot frame, the reference the delta budget is measured against.
	// Compressed when compressing, raw otherwise.
	#snapshotLen = 0;
	#groupFrames = 0;

	// Group-scoped `deflate-raw` compression. `#encoder` is the current group's stream, swapped for a
	// fresh one (cold window) at each snapshot, so a snapshot and its deltas share one DEFLATE stream.
	#compress = false;
	#encoder?: Encoder;

	constructor(track: Moq.TrackProducer, config: Config<T> = {}) {
		this.#track = track;
		this.#config = config;
		this.#compress = config.compression ?? false;
	}

	/** Publish a new value, emitting a snapshot or delta automatically. No-op if unchanged. */
	update(value: T): void {
		const valid = this.#config.schema ? this.#config.schema.parse(value) : value;

		// Serialize once; parse it back to a normalized JSON value for diffing and comparison
		// (dropping `undefined` fields, matching what lands on the wire).
		const text = JSON.stringify(valid);
		const json = JSON.parse(text);
		if (this.#last !== undefined && deepEqual(this.#last, json)) return;

		const snapshot = new TextEncoder().encode(text);
		const delta = this.#delta(json);
		if (delta && this.#group) {
			this.#deltaBytes += this.#writeDelta(this.#group, delta);
			this.#groupFrames += 1;
		} else {
			this.#snapshot(snapshot);
		}

		this.#last = json;
	}

	/**
	 * Mutate the current value in place and publish the result.
	 *
	 * The callback receives a deep clone of the last-published value, falling back to
	 * {@link Config.initial} if nothing has been published yet (throws if neither exists). Edit it in
	 * place; on return the result is published via {@link update}, a no-op if unchanged:
	 *
	 * ```ts
	 * producer.mutate((catalog) => {
	 * 	catalog.scte35 = { ... };
	 * });
	 * ```
	 *
	 * Independent owners can share a single Producer and each edit only their own keys: every call
	 * starts from the latest value, so sections compose instead of clobbering one another. Use
	 * {@link update} to replace the whole value instead.
	 */
	mutate(fn: (value: T) => void): void {
		// Start from the last-published value, falling back to the configured initial value. We
		// don't invent an empty object: mutating with nothing to start from is a usage error.
		const base = this.#last ?? this.#config.initial;
		if (base === undefined) {
			throw new Error("mutate() requires a prior update() or `initial` in the config");
		}

		const value = structuredClone(base) as T;
		fn(value);
		this.update(value);
	}

	/** Finish the track, closing any open group. */
	finish(): void {
		this.#group?.close();
		this.#group = undefined;
		this.#track.close();
	}

	// Resolved delta ratio: the configured value, or the default when unset. `0` disables deltas.
	get #deltaRatio(): number {
		return this.#config.deltaRatio ?? DEFAULT_DELTA_RATIO;
	}

	// Build a delta frame, or `undefined` to signal that a fresh snapshot should be published.
	//
	// The budget gate runs first, against the deltas already written, so rolling a new group costs no
	// merge-patch work. Since the gate excludes the frame about to be written, the delta that tips the
	// group past `ratio * snapshot` still lands: a group overshoots the budget by at most one delta.
	#delta(json: unknown): Uint8Array | undefined {
		const ratio = this.#deltaRatio;
		if (ratio === 0) return undefined;
		if (this.#last === undefined) return undefined;
		if (!this.#group || this.#groupFrames >= MAX_DELTA_FRAMES) return undefined;

		// Gate on the deltas accumulated so far (snapshot frame excluded), before computing the patch.
		if (this.#deltaBytes > ratio * this.#snapshotLen) return undefined;

		const result = diff(this.#last, json);
		if (result.forcedSnapshot) return undefined;

		return new TextEncoder().encode(JSON.stringify(result.patch));
	}

	#snapshot(snapshot: Uint8Array): void {
		// The previous group is complete; no more frames will be appended to it.
		this.#group?.close();

		const group = this.#track.appendGroup();
		this.#snapshotLen = this.#writeSnapshot(group, snapshot);
		this.#deltaBytes = 0;
		this.#groupFrames = 1;

		if (this.#deltaRatio !== 0) {
			// Keep the group open so future deltas can be appended.
			this.#group = group;
		} else {
			// Deltas disabled: one frame per group, identical to a plain JSON track.
			group.close();
			this.#group = undefined;
		}
	}

	// Write a group's snapshot (frame 0), returning the bytes written. On the compressed path this opens
	// a fresh per-group encoder (cold window), so the snapshot and its deltas share one DEFLATE stream.
	#writeSnapshot(group: Moq.Group, frame: Uint8Array): number {
		let data = frame;
		if (this.#compress) {
			this.#encoder = new Encoder();
			data = this.#encoder.frame(frame);
		}
		group.writeFrame({ data, timestamp: Time.Timestamp.now() });
		return data.length;
	}

	// Write a delta frame, compressed against the current group's encoder when compressing. Returns the
	// bytes written.
	#writeDelta(group: Moq.Group, frame: Uint8Array): number {
		let data = frame;
		if (this.#compress) {
			if (!this.#encoder) throw new Error("compressed delta requires an open group");
			data = this.#encoder.frame(frame);
		}
		group.writeFrame({ data, timestamp: Time.Timestamp.now() });
		return data.length;
	}
}
