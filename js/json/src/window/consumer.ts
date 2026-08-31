import type * as Moq from "@moq/net";

import { type ConsumerConfig, Decoder, type Event, type Group } from "./decoder.ts";

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
 *
 * @public
 */
export class Consumer<T> {
	#track: Moq.Track.Subscriber;
	#decoder: Decoder<T>;

	#group?: Moq.Group.Consumer;
	#codec?: Group;
	#reading = false;

	constructor(track: Moq.Track.Subscriber, config: ConsumerConfig = {}) {
		this.#track = track;
		this.#decoder = new Decoder(config);
	}

	/** Absolute index of the oldest record in the window. */
	get offset(): number {
		return this.#decoder.offset;
	}

	/** Get the next event, or `undefined` once the track ends. */
	next(): Promise<Event<T> | undefined> {
		if (this.#reading) throw new Error("multiple calls to next not supported");
		this.#reading = true;
		return this.#read().finally(() => {
			this.#reading = false;
		});
	}

	async #read(): Promise<Event<T> | undefined> {
		for (;;) {
			// Drain what the frames already decoded produced before reading more.
			const event = this.#decoder.next();
			if (event) return event;

			if (!this.#group) {
				this.#group = await this.#track.nextGroup();
				if (!this.#group) return undefined;
				this.#codec = this.#decoder.group();
			}

			const frame = await this.#group.readFrame();
			if (frame === undefined) {
				// This group is exhausted. Wait for a later one, which restates the window; the stream
				// ends only when the track does.
				this.#group = undefined;
				this.#codec = undefined;
				continue;
			}

			if (!this.#codec) throw new Error("an open MoQ group has a window codec");
			this.#codec.decode(frame.payload);
		}
	}

	/** Iterate over events until the track ends. */
	async *[Symbol.asyncIterator](): AsyncIterator<Event<T>> {
		for (;;) {
			const event = await this.next();
			if (event === undefined) return;
			yield event;
		}
	}
}
