import type { Timed } from "@moq/net";
import * as Moq from "@moq/net";
import { Decoder as Flate } from "../codec.ts";

import { isDeflate } from "../compression.ts";
import type { Config as CodecConfig } from "./producer.ts";

/**
 * Consumes an opaque value from a track, yielding each with the timestamp of its frame.
 *
 * Two reads: {@link next} yields every value in group order, for a caller that picks the value
 * matching a playhead, and {@link latest} jumps to the newest group, for a caller that only wants
 * the current value. What {@link latest} skips is gone, so a later {@link next} resumes after
 * it. Interoperable with the Rust `moq_flate::snapshot::Consumer`.
 */
export class Consumer {
	#track: Moq.Track.Ordered;
	#decompress: boolean;

	// The group the current window belongs to, so a boundary restarts it.
	#group?: number;
	// The DEFLATE window for the current group, present while decompressing. A snapshot group is
	// normally one frame, but the window is per group either way.
	#flate?: Flate;

	constructor(config: Consumer.Config) {
		this.#track = config.track.ordered();
		this.#decompress = isDeflate(config.compression);
	}

	/**
	 * Get the next value in group order, or `undefined` once the track ends.
	 *
	 * Buffers every value the reader has not reached yet. On a timed track, a group the
	 * subscription's `maxDelay` proves too old is skipped: the default of zero keeps only the
	 * newest group, so a playhead reader sets `maxDelay` to how far it trails the live edge. An
	 * untimed track never proves a group stale, so a reader that falls behind replays the whole
	 * backlog; use {@link latest} to skip it. A group lost to a gap resyncs from the next one.
	 */
	next(): Promise<Timed<Uint8Array> | undefined> {
		return this.#read(false);
	}

	/**
	 * Get the newest value, or `undefined` once the track ends.
	 *
	 * Every group is a complete value, so any older group is already superseded. Raising the read
	 * floor to the newest sequence before each read does two things: it discards a backlog instead
	 * of decoding every superseded value in turn, and it abandons a group a newer one has
	 * superseded rather than waiting out its close. A reader's latency therefore never grows with
	 * the queue, and never depends on a stale group's FIN arriving.
	 */
	latest(): Promise<Timed<Uint8Array> | undefined> {
		return this.#read(true);
	}

	async #read(skip: boolean): Promise<Timed<Uint8Array> | undefined> {
		for (;;) {
			if (skip) {
				const latest = this.#track.latest();
				if (latest !== undefined) this.#track.setGroups({ start: { included: latest } });
			}

			let next: Awaited<ReturnType<Moq.Track.Ordered["readFrame"]>>;
			try {
				next = await this.#track.readFrame();
			} catch (err) {
				// Falling behind a group's eviction window is recoverable: the next group carries a
				// complete value of its own, so resync there rather than surfacing a partial read.
				// Anything else is the track's terminal error, which every later read would throw
				// again; swallowing it would spin here instead of telling the caller the
				// subscription died.
				if (!(err instanceof Moq.Error.TooFarBehind)) throw err;
				continue;
			}

			if (!next) return undefined;

			// Each group is its own compressed stream, so a boundary starts a cold window.
			if (next.group !== this.#group) {
				this.#group = next.group;
				this.#flate = this.#decompress ? new Flate() : undefined;
			}

			return { value: this.#flate ? this.#flate.frame(next.payload) : next.payload, at: next.timestamp };
		}
	}

	/** Iterate over every value in group order, as {@link next} yields them, until the track ends. */
	async *[Symbol.asyncIterator](): AsyncIterator<Timed<Uint8Array>> {
		for (;;) {
			const value = await this.next();
			if (value === undefined) return;
			yield value;
		}
	}
}

export namespace Consumer {
	/** Snapshot consumer options, including the source track. */
	export type Config = CodecConfig & { track: Moq.Track.Subscriber };
}
