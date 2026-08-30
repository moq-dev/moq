import type * as Moq from "@moq/net";

import { type ConsumerConfig, Decoder, type Event } from "./decoder.ts";

/**
 * Consumes a sliding window of JSON records from a track, yielding one event per change.
 *
 * A {@link Decoder} that owns its track: it reads groups, starts a cold DEFLATE window at each
 * boundary, and turns each group's header into just the changes this reader has not been told about.
 * When something else already owns the track, use the {@link Decoder} directly.
 *
 * Group rolls never surface. A publisher rolls for compression's sake, and a header restating the
 * window yields nothing for records already delivered, so this reads as one continuous stream of
 * {@link Event}s regardless of how the publisher framed them.
 */
export class Consumer<T> {
	#track: Moq.Track.Subscriber;
	#decoder: Decoder<T>;

	#group?: Moq.Group.Consumer;

	constructor(track: Moq.Track.Subscriber, config: ConsumerConfig = {}) {
		this.#track = track;
		this.#decoder = new Decoder(config);
	}

	/** Absolute index of the oldest record in the window. */
	get offset(): number {
		return this.#decoder.offset;
	}

	/** Get the next event, or `undefined` once the track ends. */
	async next(): Promise<Event<T> | undefined> {
		for (;;) {
			// Drain what the frames already decoded produced before reading more.
			const event = this.#decoder.next();
			if (event) return event;

			if (!this.#group) {
				this.#group = await this.#track.nextGroup();
				if (!this.#group) return undefined;
				// Each group is its own compressed stream, so the window starts cold. The index cursor
				// deliberately survives, which is what makes the header report only what this reader missed.
				this.#decoder.startGroup();
			}

			const frame = await this.#group.readFrame();
			if (frame === undefined) {
				// This group is exhausted. Wait for a later one, which restates the window; the stream
				// ends only when the track does.
				this.#group = undefined;
				continue;
			}

			this.#decoder.decode(frame.payload);
		}
	}

	async *[Symbol.asyncIterator](): AsyncIterator<Event<T>> {
		for (;;) {
			const event = await this.next();
			if (event === undefined) return;
			yield event;
		}
	}
}
