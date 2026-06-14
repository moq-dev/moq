import type * as Moq from "@moq/net";

import type { Codec } from "./codec.ts";
import { encodeDelta, encodeSnapshot, INSERT, keyOf, REMOVE } from "./wire.ts";

// Maximum frames (snapshot + deltas) in a single group before a new snapshot is forced. Kept well
// below the per-group frame cap so a late joiner can always read the snapshot at frame 0.
const MAX_DELTA_FRAMES = 256;

export interface Config<T> {
	// Encodes items to and from their wire bytes. Use `stringCodec` for a set of strings.
	codec: Codec<T>;

	// A delta is appended to the current group while the deltas accumulated since the last snapshot
	// stay within `deltaRatio` times the size of a fresh snapshot; otherwise a new snapshot group is
	// started. Defaults to 2. The whole point of a set track is incremental add/remove, so deltas are
	// on by default (unlike @moq/json).
	deltaRatio?: number;
}

/** Publishes a set over a track, choosing snapshots and deltas automatically. */
export class Producer<T> {
	#track: Moq.Track;
	#codec: Codec<T>;
	#deltaRatio: number;

	// Keyed by encoded bytes so items dedupe by value, not reference.
	#items = new Map<string, T>();

	#group?: Moq.Group;
	#groupFrames = 0;
	#groupDeltaBytes = 0;

	constructor(track: Moq.Track, config: Config<T>) {
		this.#track = track;
		this.#codec = config.codec;
		this.#deltaRatio = config.deltaRatio ?? 2;
	}

	/** Insert an item, publishing a delta or snapshot. Returns true if it was newly inserted. */
	insert(value: T): boolean {
		const bytes = this.#codec.encode(value);
		const key = keyOf(bytes);
		if (this.#items.has(key)) return false;

		this.#items.set(key, value);
		this.#publish(INSERT, bytes);
		return true;
	}

	/** Remove an item, publishing a delta or snapshot. Returns true if it was present. */
	remove(value: T): boolean {
		const bytes = this.#codec.encode(value);
		const key = keyOf(bytes);
		if (!this.#items.has(key)) return false;

		this.#items.delete(key);
		this.#publish(REMOVE, bytes);
		return true;
	}

	/** Whether the item is currently in the set. */
	has(value: T): boolean {
		return this.#items.has(keyOf(this.#codec.encode(value)));
	}

	/** The number of items currently in the set. */
	get size(): number {
		return this.#items.size;
	}

	/** Iterate over the items currently in the set. */
	values(): IterableIterator<T> {
		return this.#items.values();
	}

	/** Finish the track, closing any open group. */
	finish(): void {
		this.#group?.close();
		this.#group = undefined;
		this.#track.close();
	}

	// Publish a single change. The change is already reflected in `#items`, so a snapshot captures it.
	#publish(op: number, item: Uint8Array): void {
		const snapshot = this.#snapshot();
		const deltaLen = 1 + item.length;

		if (this.#shouldSnapshot(deltaLen, snapshot.length)) {
			this.#writeSnapshot(snapshot);
		} else {
			// biome-ignore lint/style/noNonNullAssertion: shouldSnapshot returning false guarantees an open group.
			this.#group!.writeFrame(encodeDelta(op, item));
			this.#groupFrames += 1;
			this.#groupDeltaBytes += deltaLen;
		}
	}

	#snapshot(): Uint8Array {
		const items: Uint8Array[] = [];
		for (const value of this.#items.values()) items.push(this.#codec.encode(value));
		return encodeSnapshot(items);
	}

	#shouldSnapshot(deltaLen: number, snapshotLen: number): boolean {
		if (!this.#group || this.#groupFrames >= MAX_DELTA_FRAMES) return true;
		// Roll a snapshot once the replayed deltas would outgrow the budget relative to a snapshot.
		return this.#groupDeltaBytes + deltaLen > this.#deltaRatio * snapshotLen;
	}

	#writeSnapshot(snapshot: Uint8Array): void {
		// The previous group is complete; no more frames will be appended to it.
		this.#group?.close();

		const group = this.#track.appendGroup();
		group.writeFrame(snapshot);
		this.#group = group;
		this.#groupFrames = 1;
		this.#groupDeltaBytes = 0;
	}
}
