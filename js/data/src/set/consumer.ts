import type * as Moq from "@moq/net";

import type { Codec } from "./codec.ts";
import type { Config } from "./producer.ts";
import { decodeDelta, decodeSnapshot, INSERT, keyOf, REMOVE } from "./wire.ts";

/**
 * The items added and removed by a single change, returned by {@link Consumer.next}.
 *
 * A delta carries one item in exactly one field. A snapshot (the first frame of a group, or a late
 * joiner's first read) carries its difference from the previous state, so several items may be
 * added and removed at once.
 */
export interface Update<T> {
	added: T[];
	removed: T[];
}

/**
 * Consumes a set from a track, reconstructing it from snapshots and deltas.
 *
 * Each change is reduced to the items it added and removed; the full set is available via
 * {@link current}.
 */
export class Consumer<T> {
	#track: Moq.Track;
	#codec: Codec<T>;

	#group?: Moq.Group;
	// Keyed by encoded bytes so items dedupe by value, not reference.
	#current = new Map<string, T>();
	#framesRead = 0;

	constructor(track: Moq.Track, config: Config<T>) {
		this.#track = track;
		this.#codec = config.codec;
	}

	/** The full set as currently reconstructed. Updated by each {@link next}. */
	current(): Set<T> {
		return new Set(this.#current.values());
	}

	/**
	 * Get the next change as added/removed items, or undefined once the track ends.
	 *
	 * Frames that change nothing are skipped, so a returned {@link Update} is never empty. Switching
	 * to a newer group diffs its snapshot against the current set, so no change is missed or doubled.
	 */
	async next(): Promise<Update<T> | undefined> {
		for (;;) {
			if (!this.#group) {
				// Advance to the next group with a higher sequence number (skipping late arrivals). We
				// keep #current across the switch so the next snapshot diffs against it.
				this.#group = await this.#track.nextGroupOrdered();
				if (!this.#group) return undefined;
				this.#framesRead = 0;
			}

			const frame = await this.#group.readFrame();
			if (frame === undefined) {
				// The group is exhausted; advance to the next one.
				this.#group = undefined;
				continue;
			}

			const update = this.#apply(frame);
			if (update.added.length > 0 || update.removed.length > 0) return update;
			// A no-op frame (redundant snapshot or delta); read the next one.
		}
	}

	async *[Symbol.asyncIterator](): AsyncIterator<Update<T>> {
		for (;;) {
			const update = await this.next();
			if (update === undefined) return;
			yield update;
		}
	}

	// Apply one frame, returning what it changed: frame 0 of a group is a snapshot (diffed against
	// the current set), the rest are insert/remove deltas.
	#apply(frame: Uint8Array): Update<T> {
		this.#framesRead += 1;

		if (this.#framesRead === 1) {
			const next = new Map<string, T>();
			for (const item of decodeSnapshot(frame)) {
				next.set(keyOf(item), this.#codec.decode(item));
			}

			const added: T[] = [];
			const removed: T[] = [];
			for (const [key, value] of next) if (!this.#current.has(key)) added.push(value);
			for (const [key, value] of this.#current) if (!next.has(key)) removed.push(value);
			this.#current = next;
			return { added, removed };
		}

		const [op, item] = decodeDelta(frame);
		const key = keyOf(item);
		if (op === INSERT) {
			if (this.#current.has(key)) return { added: [], removed: [] };
			const value = this.#codec.decode(item);
			this.#current.set(key, value);
			return { added: [value], removed: [] };
		}
		if (op === REMOVE) {
			const value = this.#current.get(key);
			if (value === undefined) return { added: [], removed: [] };
			this.#current.delete(key);
			return { added: [], removed: [value] };
		}
		throw new Error(`unknown op byte: ${op}`);
	}
}
