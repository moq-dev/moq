import type * as Moq from "@moq/net";

import type { Codec } from "./codec.ts";
import type { Config } from "./producer.ts";
import { decodeDelta, decodeSnapshot, INSERT, keyOf, REMOVE } from "./wire.ts";

/**
 * Consumes a set from a track, reconstructing it from snapshots and deltas.
 *
 * Reads each group's snapshot (frame 0) and applies the following frames as insert/remove deltas,
 * yielding the reconstructed set after each one.
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

	/** Get the set after the next change, or undefined once the track ends. */
	async next(): Promise<Set<T> | undefined> {
		for (;;) {
			if (!this.#group) {
				// Advance to the next group with a higher sequence number (skipping late arrivals).
				this.#group = await this.#track.nextGroupOrdered();
				if (!this.#group) return undefined;
				this.#current = new Map();
				this.#framesRead = 0;
			}

			const frame = await this.#group.readFrame();
			if (frame === undefined) {
				// The group is exhausted; advance to the next one.
				this.#group = undefined;
				continue;
			}

			this.#apply(frame);
			return new Set(this.#current.values());
		}
	}

	async *[Symbol.asyncIterator](): AsyncIterator<Set<T>> {
		for (;;) {
			const value = await this.next();
			if (value === undefined) return;
			yield value;
		}
	}

	// Frame 0 of a group is a snapshot, the rest are insert/remove deltas.
	#apply(frame: Uint8Array): void {
		if (this.#framesRead === 0) {
			this.#current = new Map();
			for (const item of decodeSnapshot(frame)) {
				this.#current.set(keyOf(item), this.#codec.decode(item));
			}
		} else {
			const [op, item] = decodeDelta(frame);
			const key = keyOf(item);
			if (op === INSERT) {
				this.#current.set(key, this.#codec.decode(item));
			} else if (op === REMOVE) {
				this.#current.delete(key);
			} else {
				throw new Error(`unknown op byte: ${op}`);
			}
		}
		this.#framesRead += 1;
	}
}
