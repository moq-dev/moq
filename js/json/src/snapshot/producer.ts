import { Encoder } from "@moq/flate";
import * as Moq from "@moq/net";
import type { Effect } from "@moq/signals";
import type * as z from "zod/mini";

import { deepEqual, diff } from "../diff.ts";

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

/**
 * Publishes a JSON value as snapshots and deltas, chosen automatically.
 *
 * Construct it two ways:
 *
 * - **With a track** (`new Producer(track, config)`): writes directly to that one track.
 * - **Without a track** (`new Producer(config)`): retains the value and fans it out to any number of
 *   subscription tracks attached with {@link serve}, seeding late joiners with the current value.
 *   This backs the hang catalog and is how an application publishes its own custom tracks.
 */
export class Producer<T> {
	#config: Config<T>;

	// Leaf mode: writes snapshots/deltas straight to a single track.
	#track?: Moq.Track;
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

	// Fan-out mode: retains the value and serves a child (leaf) Producer per subscriber.
	#outputs?: Set<Producer<T>>;
	#value?: T;

	/** Create a track-less, fan-out producer; attach subscribers with {@link serve}. */
	constructor(config?: Config<T>);
	/** Create a producer that writes directly to `track`. */
	constructor(track: Moq.Track, config?: Config<T>);
	constructor(trackOrConfig?: Moq.Track | Config<T>, config: Config<T> = {}) {
		if (trackOrConfig instanceof Moq.Track) {
			this.#track = trackOrConfig;
			this.#config = config;
		} else {
			this.#config = trackOrConfig ?? {};
			this.#outputs = new Set();
			this.#value = this.#config.initial;
		}
		this.#compress = this.#config.compression ?? false;
	}

	/** The current value, or `undefined` if nothing has been published yet. */
	get value(): T | undefined {
		return this.#track ? (this.#last as T | undefined) : this.#value;
	}

	/** Publish a new value, emitting a snapshot or delta automatically. No-op if unchanged. */
	update(value: T): void {
		if (!this.#track) {
			// Fan-out: retain the value and forward it to every subscriber. Isolate per-subscriber
			// failures so one broken track (e.g. closed mid-update) doesn't stop the others.
			this.#value = value;
			for (const output of this.#outputs ?? []) {
				try {
					output.update(value);
				} catch (err) {
					this.#outputs?.delete(output);
					try {
						output.finish();
					} catch {
						// Already broken; nothing more to do.
					}
					console.warn("dropping failed json subscriber during fan-out", err);
				}
			}
			return;
		}

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
			this.#snapshot(this.#track, snapshot);
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
		const base = (this.#track ? this.#last : this.#value) ?? this.#config.initial;
		if (base === undefined) {
			throw new Error("mutate() requires a prior update() or `initial` in the config");
		}

		const value = structuredClone(base) as T;
		fn(value);
		this.update(value);
	}

	/**
	 * Serve a subscription request: seed the track with the current value, then forward updates.
	 *
	 * Only available on a track-less (fan-out) producer. The subscriber is removed and finished when
	 * `effect` is cleaned up.
	 *
	 * Pass `opts.compression` to override the producer's configured compression for this subscriber
	 * only, so one fan-out producer can serve the same value both plaintext and `deflate-raw` (e.g.
	 * the catalog served on `catalog.json` and `catalog.json.z`).
	 */
	serve(track: Moq.Track, effect: Effect, opts?: { compression?: boolean }): void {
		if (!this.#outputs) {
			throw new Error("serve() is only available on a track-less Producer");
		}

		const config =
			opts?.compression === undefined ? this.#config : { ...this.#config, compression: opts.compression };
		const output = new Producer<T>(track, config);
		if (this.#value !== undefined) output.update(this.#value);

		this.#outputs.add(output);
		effect.cleanup(() => {
			this.#outputs?.delete(output);
			output.finish();
		});
	}

	/** Finish: close the track (leaf) or finish every subscriber (fan-out). */
	finish(): void {
		if (!this.#track) {
			for (const output of this.#outputs ?? []) output.finish();
			this.#outputs?.clear();
			return;
		}

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

	#snapshot(track: Moq.Track, snapshot: Uint8Array): void {
		// The previous group is complete; no more frames will be appended to it.
		this.#group?.close();

		const group = track.appendGroup();
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
		if (!this.#compress) {
			group.writeFrame(frame);
			return frame.length;
		}
		this.#encoder = new Encoder();
		const slice = this.#encoder.frame(frame);
		group.writeFrame(slice);
		return slice.length;
	}

	// Write a delta frame, compressed against the current group's encoder when compressing. Returns the
	// bytes written.
	#writeDelta(group: Moq.Group, frame: Uint8Array): number {
		if (!this.#compress) {
			group.writeFrame(frame);
			return frame.length;
		}
		if (!this.#encoder) throw new Error("compressed delta requires an open group");
		const slice = this.#encoder.frame(frame);
		group.writeFrame(slice);
		return slice.length;
	}
}
