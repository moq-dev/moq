import type * as Moq from "@moq/net";
import type * as z from "zod/mini";

import { deepEqual, diff } from "./diff.ts";

// Maximum frames (snapshot + deltas) in a single group before a new snapshot is forced. Kept
// well below the per-group frame cap so a late joiner can always read the snapshot at frame 0.
const MAX_DELTA_FRAMES = 256;

export interface Config {
	// Controls whether the producer emits deltas (merge patches) instead of full snapshots.
	//
	// `undefined` disables deltas: every change is published as a new snapshot group.
	//
	// A number enables deltas: a delta is appended to the current group as long as the group's
	// total size stays within `maxDeltaRatio` times the size of a fresh snapshot; otherwise a
	// new snapshot group is started.
	maxDeltaRatio?: number;
}

/** Publishes a JSON value over a track, choosing snapshots and deltas automatically. */
export class Producer<T> {
	#track: Moq.Track;
	#config: Config;
	#schema?: z.ZodMiniType<T>;

	#group?: Moq.Group;
	#last?: unknown;
	#groupBytes = 0;
	#groupFrames = 0;

	constructor(track: Moq.Track, config: Config = {}, schema?: z.ZodMiniType<T>) {
		this.#track = track;
		this.#config = config;
		this.#schema = schema;
	}

	/** Publish a new value, emitting a snapshot or delta automatically. No-op if unchanged. */
	update(value: T): void {
		const valid = this.#schema ? this.#schema.parse(value) : value;

		// Serialize once; parse it back to a normalized JSON value for diffing and comparison
		// (dropping `undefined` fields, matching what lands on the wire).
		const text = JSON.stringify(valid);
		const json = JSON.parse(text);
		if (this.#last !== undefined && deepEqual(this.#last, json)) return;

		const snapshot = new TextEncoder().encode(text);
		const delta = this.#delta(json, snapshot.length);
		if (delta) {
			// biome-ignore lint/style/noNonNullAssertion: a delta is only produced with an open group.
			const group = this.#group!;
			group.writeFrame(delta);
			this.#groupBytes += delta.length;
			this.#groupFrames += 1;
		} else {
			this.#snapshot(snapshot);
		}

		this.#last = json;
	}

	/** Finish the track, closing any open group. */
	finish(): void {
		this.#group?.close();
		this.#group = undefined;
		this.#track.close();
	}

	#delta(json: unknown, snapshotLen: number): Uint8Array | undefined {
		const ratio = this.#config.maxDeltaRatio;
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

		if (this.#config.maxDeltaRatio !== undefined) {
			// Keep the group open so future deltas can be appended.
			this.#group = group;
		} else {
			// Deltas disabled: one frame per group, identical to a plain JSON track.
			group.close();
			this.#group = undefined;
		}
	}
}
