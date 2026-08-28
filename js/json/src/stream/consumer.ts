import type * as Moq from "@moq/net";

import { type ConsumerConfig, Decoder } from "./decoder.ts";

/**
 * Thrown by a stream read when the track carried a second group, which a lossless log cannot do.
 *
 * A stream is a single group by construction: a publisher that cannot write a record ends the
 * track rather than rolling. A second group therefore means whatever would have completed the
 * first is gone, so the read reports it instead of handing back the remainder as a continuous log.
 *
 * Mirrors the Rust `moq_json::Error::Rolled`.
 */
export class Rolled extends Error {
	constructor() {
		super("rolled: the stream carried a second group, so records are missing");
		this.name = "Rolled";
	}
}

/**
 * Consumes an ordered log of JSON records from a track, yielding every record in order.
 *
 * A {@link Decoder} that owns its track, reading one record per frame. The log is a single group,
 * which is what makes the mode lossless: rolling to a second group means the records that would
 * have completed the first are gone, so a {@link Producer} that cannot write ends the track
 * instead. A second group is therefore a broken publisher, and reading it would present a gap as a
 * continuous log, so it throws {@link Rolled} rather than yielding the remainder. When something
 * else already owns the track, use the {@link Decoder} directly.
 */
export class Consumer<T> {
	#track: Moq.Track.Subscriber;
	#decoder: Decoder<T>;

	#group?: Moq.Group.Consumer;
	// Whether the log's one group has been taken, so a second is a rolled log rather than the first.
	#taken = false;

	constructor(track: Moq.Track.Subscriber, config: ConsumerConfig = {}) {
		this.#track = track;
		this.#decoder = new Decoder(config);
	}

	/** Get the next record, or `undefined` once the track ends. */
	async next(): Promise<T | undefined> {
		for (;;) {
			if (!this.#group) {
				// Arrival order rather than sequence order, because there is only ever one group to
				// take and a second one has to be seen whatever its sequence. The monotonic
				// `nextGroup` would drop a late lower sequence, which is the very loss this reports.
				this.#group = await this.#track.recvGroup();
				if (!this.#group) return undefined;
				if (this.#taken) throw new Rolled();
				this.#taken = true;
				// Each group is its own compressed stream, so the window starts cold.
				this.#decoder.reset();
			}

			const frame = await this.#group.readFrame();
			if (frame === undefined) {
				// The log's one group is exhausted. Keep reading the track so a clean end still
				// reports the log as complete, and so a second group is caught as Rolled.
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
