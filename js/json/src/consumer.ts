import type * as Moq from "@moq/net";
import type * as z from "zod/mini";
import { Decoder } from "./compression.ts";
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
	// Whether frames are `deflate-raw` compressed. Must match the producer's {@link Config.compression}.
	#decompress: boolean;

	#group?: Moq.Group;
	// Per-group DEFLATE decoder, built lazily on the first frame of a group and reset at each boundary.
	#decoder?: Decoder;
	#current?: unknown;
	#framesRead = 0;

	constructor(track: Moq.Track, config: Config<T> = {}) {
		this.#track = track;
		this.#schema = config.schema;
		this.#decompress = config.compression ?? false;
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
				// Each group is its own compressed stream, so start a fresh decoder.
				this.#decoder = undefined;
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

	// Frame 0 of a group is a snapshot, the rest are merge patches. When compressed, frames share one
	// per-group DEFLATE stream, so they decode in order through a decoder built on the group's first frame.
	#apply(frame: Uint8Array): T {
		let payload = frame;
		if (this.#decompress) {
			this.#decoder ??= new Decoder();
			payload = this.#decoder.frame(frame);
		}
		const parsed = JSON.parse(new TextDecoder().decode(payload));
		if (this.#framesRead === 0) {
			this.#current = parsed;
		} else {
			this.#current = merge(this.#current, parsed);
		}
		this.#framesRead += 1;

		return this.#schema ? this.#schema.parse(this.#current) : (this.#current as T);
	}
}
