import type * as Moq from "@moq/net";
import type * as z from "zod/mini";
import { merge } from "./diff.ts";
import type { Config } from "./producer.ts";

/**
 * Consumes a JSON value from a track, reconstructing it from snapshots and deltas.
 *
 * Reads each group's snapshot (frame 0) and applies the following frames as merge patches,
 * yielding the reconstructed value after each one.
 */
export class Consumer<T> {
	#track: Moq.Track;
	#schema?: z.ZodMiniType<T>;

	#group?: Moq.Group;
	#current?: unknown;
	#framesRead = 0;

	constructor(track: Moq.Track, config: Config<T> = {}) {
		this.#track = track;
		this.#schema = config.schema;
	}

	/** Get the next reconstructed value, or `undefined` once the track ends. */
	async next(): Promise<T | undefined> {
		for (;;) {
			if (!this.#group) {
				// Advance to the next group with a higher sequence number (skipping late arrivals).
				this.#group = await this.#track.nextGroupOrdered();
				if (!this.#group) return undefined;
				this.#current = undefined;
				this.#framesRead = 0;
			}

			const frame = await this.#group.readFrame();
			if (frame === undefined) {
				// The group is exhausted; advance to the next one.
				this.#group = undefined;
				continue;
			}

			return this.#apply(frame);
		}
	}

	async *[Symbol.asyncIterator](): AsyncIterator<T> {
		for (;;) {
			const value = await this.next();
			if (value === undefined) return;
			yield value;
		}
	}

	// Frame 0 of a group is a snapshot, the rest are merge patches.
	#apply(frame: Uint8Array): T {
		const parsed = JSON.parse(new TextDecoder().decode(frame));
		if (this.#framesRead === 0) {
			this.#current = parsed;
		} else {
			this.#current = merge(this.#current, parsed);
		}
		this.#framesRead += 1;

		return this.#schema ? this.#schema.parse(this.#current) : (this.#current as T);
	}
}
