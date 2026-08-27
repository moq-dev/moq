import { Decoder as Flate } from "@moq/flate";
import type * as Moq from "@moq/net";

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
	#track: Moq.Track.Subscriber;
	#decompress: boolean;

	#group?: Moq.Group.Consumer;
	// The DEFLATE window for the current group, present while decompressing. A snapshot group is
	// normally one frame, but the window is per group either way.
	#flate?: Flate;

	constructor(track: Moq.Track.Subscriber, config: ConsumerConfig = {}) {
		this.#track = track;
		this.#decompress = config.compression ?? false;
	}

	/**
	 * Get the next value, or `undefined` once the track ends.
	 *
	 * Skips straight to the newest group, so a late joiner (or a consumer that has fallen behind)
	 * starts at the current value instead of replaying superseded ones, and its latency never grows
	 * with the backlog. Within that group everything already buffered is drained and only the last
	 * value yielded; a compressed group's frames are still decoded in order, since they share one
	 * window.
	 *
	 * This consumer owns its subscriber's read cursor, which is what lets it discard the backlog.
	 */
	async next(): Promise<Uint8Array | undefined> {
		for (;;) {
			if (!this.#group) {
				// Every group is a complete value, so a buffered older one is already superseded.
				// Raising the floor to the newest sequence drops them instead of decoding each in turn.
				const latest = this.#track.latest();
				if (latest !== undefined) this.#track.startAt(latest);

				// Advance to the next group with a higher sequence number (skipping late arrivals).
				this.#group = await this.#track.nextGroup();
				if (!this.#group) return undefined;
				// Each group is its own compressed stream, so the window starts cold.
				this.#flate = this.#decompress ? new Flate() : undefined;
			}

			let latest: Uint8Array | undefined;
			for (let frame = this.#group.tryReadFrame(); frame !== undefined; frame = this.#group.tryReadFrame()) {
				latest = this.#decode(frame.payload);
			}
			if (latest !== undefined) return latest;

			// Nothing buffered: block for the next frame (or the group's end).
			let frame: Moq.Group.Frame | undefined;
			try {
				frame = await this.#group.readFrame();
			} catch {
				// The group was reset or we fell behind its eviction window. Resync from the next group,
				// which carries a complete value of its own, so no partial state is presented.
				this.#group = undefined;
				continue;
			}

			if (frame === undefined) {
				// The group is exhausted; wait for a newer one.
				this.#group = undefined;
				continue;
			}

			return this.#decode(frame.payload);
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

	// Decompress one frame, if the track is compressed.
	#decode(payload: Uint8Array): Uint8Array {
		return this.#flate ? this.#flate.frame(payload) : payload;
	}
}
