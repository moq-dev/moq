import type * as Moq from "@moq/net";
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

	// Starting value for {@link Producer.lock} before anything has been published. Required to lock
	// a producer that hasn't published yet (e.g. a fresh catalog); ignored once a value exists.
	initial?: T;
}

/** A disposable editing guard returned by {@link Producer.lock}, publishing its `value` on dispose. */
export interface Guard<T> extends Disposable {
	value: T;
}

/** Publishes a JSON value over a track, choosing snapshots and deltas automatically. */
export class Producer<T> {
	#track: Moq.Track;
	#config: Config<T>;

	#group?: Moq.Group;
	#last?: unknown;
	#groupBytes = 0;
	#groupFrames = 0;

	constructor(track: Moq.Track, config: Config<T> = {}) {
		this.#track = track;
		this.#config = config;
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
		const delta = this.#delta(json, snapshot.length);
		if (delta && this.#group) {
			this.#group.writeFrame(delta);
			this.#groupBytes += delta.length;
			this.#groupFrames += 1;
		} else {
			this.#snapshot(snapshot);
		}

		this.#last = json;
	}

	/**
	 * Lock the current value for in-place editing, publishing on dispose.
	 *
	 * Returns a disposable guard whose `value` is a deep clone of the last-published value, falling
	 * back to {@link Config.initial} if nothing has been published yet; throws if neither exists.
	 * Edit `value` in place; when the guard is disposed it publishes the result via {@link update},
	 * a no-op if unchanged. Use it with `using` so the publish happens at the end of the block:
	 *
	 * ```ts
	 * using catalog = producer.lock();
	 * catalog.value.scte35 = { ... };
	 * ```
	 *
	 * Independent owners can share a single Producer and each edit only their own keys: every lock
	 * starts from the latest value, so sections compose instead of clobbering one another. Hold at
	 * most one guard at a time. Use {@link update} to replace the whole value instead.
	 */
	lock(): Guard<T> {
		// Start from the last-published value, falling back to the configured initial value. We
		// don't invent an empty object: locking with nothing to start from is a usage error.
		const base = this.#last ?? this.#config.initial;
		if (base === undefined) {
			throw new Error("lock() requires a prior update() or `initial` in the config");
		}

		const value = structuredClone(base) as T;
		return {
			value,
			[Symbol.dispose]: () => this.update(value),
		};
	}

	/** Finish the track, closing any open group. */
	finish(): void {
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

	#snapshot(snapshot: Uint8Array): void {
		// The previous group is complete; no more frames will be appended to it.
		this.#group?.close();

		const group = this.#track.appendGroup();
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
