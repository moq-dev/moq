import { Decoder as Flate } from "@moq/flate";
import * as Moq from "@moq/net";

/** Options for a {@link Consumer}. */
export interface ConsumerConfig {
	/** Whether the frames are `deflate-raw` compressed. Must match the producer. Defaults to `false`. */
	compression?: boolean;
}

/**
 * Consumes a binary value from a track, yielding the newest one.
 *
 * Jumps to the newest group and reads the value out of it, so a late joiner starts at the current
 * value rather than replaying superseded ones. Interoperable with the Rust
 * `moq_binary::snapshot::Consumer`, which collapses the same backlog.
 */
export class Consumer {
	#track: Moq.Track.Ordered;
	#decompress: boolean;

	// The group the current window belongs to, so a boundary restarts it.
	#group?: number;
	// The DEFLATE window for the current group, present while decompressing. A snapshot group is
	// normally one frame, but the window is per group either way.
	#flate?: Flate;

	constructor(track: Moq.Track.Subscriber, config: ConsumerConfig = {}) {
		this.#track = track.ordered();
		this.#decompress = config.compression ?? false;
	}

	/**
	 * Get the next value, or `undefined` once the track ends.
	 *
	 * Every group is a complete value, so any older group is already superseded. Raising the read
	 * floor to the newest sequence before each read does two things: it discards a backlog instead
	 * of decoding every superseded value in turn, and it abandons a group a newer one has
	 * superseded rather than waiting out its close. A snapshot reader's latency therefore never
	 * grows with the queue, and never depends on a stale group's FIN arriving.
	 *
	 * This consumer owns its subscriber's read cursor, which is what lets it discard the backlog.
	 */
	async next(): Promise<Uint8Array | undefined> {
		for (;;) {
			const latest = this.#track.latest();
			if (latest !== undefined) this.#track.startAt(latest);

			let next: Awaited<ReturnType<Moq.Track.Ordered["readFrameSequence"]>>;
			try {
				next = await this.#track.readFrameSequence();
			} catch (err) {
				// Falling behind a group's eviction window is recoverable: the next group carries a
				// complete value of its own, so resync there rather than surfacing a partial read.
				// Anything else is the track's terminal error, which every later read would throw
				// again; swallowing it would spin here instead of telling the caller the
				// subscription died.
				if (!(err instanceof Moq.Group.Lagged)) throw err;
				continue;
			}

			if (!next) return undefined;

			// Each group is its own compressed stream, so a boundary starts a cold window.
			if (next.group !== this.#group) {
				this.#group = next.group;
				this.#flate = this.#decompress ? new Flate() : undefined;
			}

			return this.#flate ? this.#flate.frame(next.payload) : next.payload;
		}
	}

	/** Iterate over values until the track ends, each the newest at the time it is yielded. */
	async *[Symbol.asyncIterator](): AsyncIterator<Uint8Array> {
		for (;;) {
			const value = await this.next();
			if (value === undefined) return;
			yield value;
		}
	}
}
