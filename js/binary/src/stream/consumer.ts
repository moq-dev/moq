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
 * A conforming publisher writes the whole log into one group, but one that writes more is read here
 * too: each group starts a cold decompression window.
 *
 * Groups are taken in arrival order (`recvGroup`), not by skipping to the newest, so nothing is
 * dropped if a publisher does use more than one. The monotonic `nextGroup` would discard a lower
 * sequence that arrived late, which for a lossless log means records nothing will repeat.
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
				// Arrival order, so a group that lands late is still delivered.
				this.#group = await this.#track.recvGroup();
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
