import { Decoder as Flate } from "@moq/flate";
import type * as Moq from "@moq/net";

/** Options for a {@link Consumer}. */
export interface ConsumerConfig {
	/** Whether the frames are `deflate-raw` compressed. Must match the producer. Defaults to `false`. */
	compression?: boolean;
}

/**
 * Consumes an ordered log of binary payloads from a track, yielding every one in order.
 *
 * A {@link Producer} writes the whole log into one group, but a publisher that rolls its own (the
 * way a failed write is recovered) is read here too: each group starts a cold decompression window.
 */
export class Consumer {
	#track: Moq.Track.Subscriber;
	#decompress: boolean;

	#group?: Moq.Group.Consumer;
	// The DEFLATE window for the current group, present while decompressing.
	#flate?: Flate;

	constructor(track: Moq.Track.Subscriber, config: ConsumerConfig = {}) {
		this.#track = track;
		this.#decompress = config.compression ?? false;
	}

	/** Get the next payload, or `undefined` once the track ends. */
	async next(): Promise<Uint8Array | undefined> {
		for (;;) {
			if (!this.#group) {
				this.#group = await this.#track.nextGroup();
				if (!this.#group) return undefined;
				// Each group is its own compressed stream, so the window starts cold.
				this.#flate = this.#decompress ? new Flate() : undefined;
			}

			const frame = await this.#group.readFrame();
			if (frame === undefined) {
				// This group is exhausted. Clear it and wait for a later one, which starts its own
				// window; the log ends only when the track does.
				this.#group = undefined;
				continue;
			}

			return this.#flate ? this.#flate.frame(frame.payload) : frame.payload;
		}
	}

	/** Iterate over every payload in order, until the track ends. */
	async *[Symbol.asyncIterator](): AsyncIterator<Uint8Array> {
		for (;;) {
			const value = await this.next();
			if (value === undefined) return;
			yield value;
		}
	}
}
