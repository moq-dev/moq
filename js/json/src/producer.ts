import * as Moq from "@moq/net";
import type { Effect } from "@moq/signals";
import type * as z from "zod/mini";

import { deepEqual, diff } from "./diff.ts";

// Maximum frames (snapshot + deltas) in a single group before a new snapshot is forced. Kept
// well below the per-group frame cap so a late joiner can always read the snapshot at frame 0.
const MAX_DELTA_FRAMES = 256;

export interface Config<T> {
	// Controls whether the producer emits deltas (merge patches) instead of full snapshots.
	//
	// `undefined` disables deltas: every change is published as a new snapshot group.
	//
	// A number enables deltas: a delta is appended to the current group as long as the group's
	// total size stays within `deltaRatio` times the size of a fresh snapshot; otherwise a new
	// snapshot group is started.
	deltaRatio?: number;

	// Optional zod schema used to validate each value before publishing.
	schema?: z.ZodMiniType<T>;

	// Starting value for {@link Producer.mutate} before anything has been published. Required to
	// mutate a producer that hasn't published yet (e.g. a fresh catalog); ignored once a value exists.
	initial?: T;
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
	#groupBytes = 0;
	#groupFrames = 0;

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
		const delta = this.#delta(json, snapshot.length);
		if (delta && this.#group) {
			this.#group.writeFrame(delta);
			this.#groupBytes += delta.length;
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
	 */
	serve(track: Moq.Track, effect: Effect): void {
		if (!this.#outputs) {
			throw new Error("serve() is only available on a track-less Producer");
		}

		const output = new Producer<T>(track, this.#config);
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

	#delta(json: unknown, snapshotLen: number): Uint8Array | undefined {
		const ratio = this.#config.deltaRatio;
		if (ratio === undefined) return undefined;
		if (this.#last === undefined) return undefined;
		if (!this.#group || this.#groupFrames >= MAX_DELTA_FRAMES) return undefined;

		const result = diff(this.#last, json);
		if (result.forcedSnapshot) return undefined;

		const delta = new TextEncoder().encode(JSON.stringify(result.patch));

		// Roll a snapshot if appending the delta would bloat the group past the budget.
		if (this.#groupBytes + delta.length > ratio * snapshotLen) return undefined;

		return delta;
	}

	#snapshot(track: Moq.Track, snapshot: Uint8Array): void {
		// The previous group is complete; no more frames will be appended to it.
		this.#group?.close();

		const group = track.appendGroup();
		group.writeFrame(snapshot);
		this.#groupBytes = snapshot.length;
		this.#groupFrames = 1;

		if (this.#config.deltaRatio !== undefined) {
			// Keep the group open so future deltas can be appended.
			this.#group = group;
		} else {
			// Deltas disabled: one frame per group, identical to a plain JSON track.
			group.close();
			this.#group = undefined;
		}
	}
}
