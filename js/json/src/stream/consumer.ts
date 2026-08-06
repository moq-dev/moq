import type * as Moq from "@moq/net";

import { type ConsumerConfig, Decoder } from "./decoder.ts";

/**
 * Consumes an ordered log of JSON records from a track, yielding every record in order.
 *
 * A {@link Decoder} that owns its track. The log rides a single group, so this reads that group's
 * frames in order; one record per frame. When something else already owns the track, use the
 * {@link Decoder} directly.
 */
export class Consumer<T> {
	#track: Moq.Track.Subscriber;
	#decoder: Decoder<T>;

	#group?: Moq.Group.Consumer;

	constructor(track: Moq.Track.Subscriber, config: ConsumerConfig = {}) {
		this.#track = track;
		this.#decoder = new Decoder(config);
	}

	/** Get the next record, or `undefined` once the track ends. */
	async next(): Promise<T | undefined> {
		for (;;) {
			if (!this.#group) {
				this.#group = await this.#track.nextGroup();
				if (!this.#group) return undefined;
				// Each group is its own compressed stream, so the window starts cold.
				this.#decoder.reset();
			}

			const frame = await this.#group.readFrame();
			if (frame === undefined) {
				// The group is finished; the log rides just this one, so the stream ends.
				this.#group = undefined;
				continue;
			}

			return this.#decoder.decode(frame.payload);
		}
	}

	async *[Symbol.asyncIterator](): AsyncIterator<T> {
		for (;;) {
			const value = await this.next();
			if (value === undefined) return;
			yield value;
		}
	}
}
