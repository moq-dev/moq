import { Decoder as Flate } from "@moq/flate";
import type * as Moq from "@moq/net";

/** Options for a {@link Consumer}. */
export interface ConsumerConfig {
	/** Whether the frames are `deflate-raw` compressed. Must match the producer. Defaults to `false`. */
	compression?: boolean;
}

/**
 * Thrown by a stream read when the track carried a second group, which a lossless log cannot do.
 *
 * A stream is a single group by construction: a publisher that cannot write a payload ends the
 * track rather than rolling. A second group therefore means whatever would have completed the
 * first is gone, so the read reports it instead of handing back the remainder as a continuous log.
 *
 * Mirrors the Rust `moq_binary::Error::Rolled`.
 */
export class Rolled extends Error {
	constructor() {
		super("rolled: the stream carried a second group, so records are missing");
		this.name = "Rolled";
	}
}

/**
 * Consumes an ordered log of binary payloads from a track, yielding every one in order.
 *
 * The log is a single group. That is what makes the mode lossless: rolling to a second group means
 * the payloads that would have completed the first are gone, so a publisher that cannot write ends
 * the track instead. A second group is therefore a broken publisher, and reading it would present
 * a gap as a continuous log, so it throws {@link Rolled} rather than yielding the remainder.
 */
export class Consumer {
	#track: Moq.Track.Subscriber;
	#decompress: boolean;

	#group?: Moq.Group.Consumer;
	// Whether the log's one group has been taken, so a second is a rolled log rather than the first.
	#taken = false;
	// The DEFLATE window for the group, present while decompressing.
	#flate?: Flate;

	constructor(track: Moq.Track.Subscriber, config: ConsumerConfig = {}) {
		this.#track = track;
		this.#decompress = config.compression ?? false;
	}

	/** Get the next payload, or `undefined` once the track ends. */
	async next(): Promise<Uint8Array | undefined> {
		for (;;) {
			if (!this.#group) {
				// Arrival order rather than sequence order, because there is only ever one group to
				// take and a second one has to be seen whatever its sequence. The monotonic
				// `nextGroup` would drop a late lower sequence, which is the very loss this reports.
				this.#group = await this.#track.recvGroup();
				if (!this.#group) return undefined;
				if (this.#taken) throw new Rolled();
				this.#taken = true;
				this.#flate = this.#decompress ? new Flate() : undefined;
			}

			const frame = await this.#group.readFrame();
			if (frame === undefined) {
				// The log's one group is exhausted. Keep reading the track so a clean end still
				// reports the log as complete, and so a second group is caught as Rolled.
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
