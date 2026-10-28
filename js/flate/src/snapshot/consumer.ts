import type { Timed } from "@moq/net";
import * as Moq from "@moq/net";
import { Decoder as Flate } from "../codec.ts";

import { isDeflate } from "../compression.ts";
import type { Config as CodecConfig } from "./producer.ts";

// Group errors that lose only that group, matching the JSON snapshot consumer.
const GAPS: Moq.StreamCode[] = [
	Moq.StreamCode.TooFarBehind,
	Moq.StreamCode.Old,
	Moq.StreamCode.Evicted,
	Moq.StreamCode.GroupTooLarge,
];

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

	// The group being read.
	#group?: Moq.Group.Consumer;
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
	 * Buffers every value the reader has not reached yet. A group the subscription's `maxDelay`
	 * proves too old is skipped, counting media time and wall time, or wall time alone for an
	 * untimed track. The default of zero keeps only the newest group, so a playhead reader sets
	 * `maxDelay` to how far it trails the live edge. Use {@link latest} to skip straight to the
	 * head. A group lost to a gap resyncs from the next one.
	 */
	next(): Promise<Timed<Uint8Array> | undefined> {
		return this.#read(false);
	}

	/**
	 * Get the newest value, or `undefined` once the track ends.
	 *
	 * Every group is a complete value, so any older group is already superseded. Jumping to the
	 * newest group the reader holds before each read does two things: it discards a backlog
	 * instead of decoding every superseded value in turn, and it abandons a group a newer one has
	 * superseded rather than waiting out its close. A reader's latency therefore never grows with
	 * the queue, and never depends on a stale group's FIN arriving.
	 */
	latest(): Promise<Timed<Uint8Array> | undefined> {
		return this.#read(true);
	}

	async #read(skip: boolean): Promise<Timed<Uint8Array> | undefined> {
		for (;;) {
			if (skip) {
				// Jump to the newest group the reader already has; a newer one supersedes the rest.
				// Taking groups, not the track's largest position, ignores datagrams (which share
				// the sequence space) and stays inside any range the caller set on the reader.
				// Mirrors the Rust consumer.
				for (let group = this.#track.tryNextGroup(); group; group = this.#track.tryNextGroup()) {
					this.#open(group);
				}
			}

			if (!this.#group) {
				const group = await this.#track.nextGroup();
				if (!group) return undefined;
				this.#open(group);
			}
			const group = this.#group;
			if (!group) continue;

			let next: Moq.Group.Frame | undefined;
			try {
				next = await group.readFrame();
			} catch (err) {
				// Losing one group (reset, or fallen behind its eviction window) is recoverable: the
				// next group carries a complete value of its own, so resync there rather than
				// surfacing a partial read. Anything else is the track's terminal error, which every
				// later read would throw again; swallowing it would spin here instead of telling the
				// caller the subscription died.
				if (!(err instanceof Moq.Error.Stream && GAPS.includes(err.code))) throw err;
				this.#group = undefined;
				continue;
			}

			if (!next) {
				this.#group = undefined;
				continue;
			}

			return { value: this.#flate ? this.#flate.frame(next.payload) : next.payload, at: next.timestamp };
		}
	}

	// Each group is its own compressed stream, so a boundary starts a cold window.
	#open(group: Moq.Group.Consumer): void {
		this.#group?.close();
		this.#group = group;
		this.#flate = this.#decompress ? new Flate() : undefined;
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

/** Types for {@link Consumer}. */
export namespace Consumer {
	/** Snapshot consumer options, including the source track. */
	export type Config = CodecConfig & { track: Moq.Track.Subscriber };
}
