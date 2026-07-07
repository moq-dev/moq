/**
 * Append-log JSON publishing over MoQ tracks: the counterpart to the snapshot/delta ("object")
 * mode in {@link ./producer.ts}. Instead of one value updated over time, a stream is an ordered
 * log of self-contained records; every {@link Producer.append} writes one JSON object as one
 * frame, and a {@link Consumer} yields them all in order.
 *
 * The whole log rides a **single group** that is never rolled: with {@link ProducerConfig.compression}
 * on, that one group is one DEFLATE window, so every record compresses against the earlier ones.
 * There is deliberately no group rolling (and so no catch-up machinery); a caller that wants to
 * bound the record rate throttles at the source. Interoperable on the wire with the Rust
 * `moq_json::stream`.
 *
 * @module
 */

import { Decoder, Encoder } from "@moq/flate";
import type * as Moq from "@moq/net";

/** Options for a stream {@link Producer}. */
export interface ProducerConfig {
	/**
	 * Compress the group as one sync-flushed `deflate-raw` stream, so each record reuses the earlier
	 * ones as context and shrinks sharply. A {@link Consumer} reading the track must set the same
	 * flag. Defaults to `false`.
	 */
	compression?: boolean;
}

/** Options for a stream {@link Consumer}. */
export interface ConsumerConfig {
	/** Whether the track's frames are `deflate-raw` compressed. Must match the producer. Defaults to `false`. */
	compression?: boolean;
}

/**
 * Publishes an ordered log of JSON records to a track, one record per frame in a single group.
 */
export class Producer<T> {
	#track: Moq.Track;
	#compress: boolean;

	// The single group carrying the whole log, opened on the first append.
	#group?: Moq.Group;
	// The group's DEFLATE encoder (one window for the whole log), present while compressing.
	#encoder?: Encoder;

	/** Wrap a track to publish a record log into it. */
	constructor(track: Moq.Track, config: ProducerConfig = {}) {
		this.#track = track;
		this.#compress = config.compression ?? false;
	}

	/** Append one record to the log. */
	append(value: T): void {
		const payload = new TextEncoder().encode(JSON.stringify(value));

		if (!this.#group) {
			this.#group = this.#track.appendGroup();
			this.#encoder = this.#compress ? new Encoder() : undefined;
		}

		this.#group.writeFrame(this.#encoder ? this.#encoder.frame(payload) : payload);
	}

	/** Finish the track, closing the group. */
	finish(): void {
		this.#group?.close();
		this.#group = undefined;
		this.#track.close();
	}
}

/**
 * Consumes an ordered log of JSON records from a track, yielding every record in order.
 *
 * The log rides a single group, so this reads that group's frames in order; one record per frame.
 */
export class Consumer<T> {
	#track: Moq.Track;
	#decompress: boolean;

	#group?: Moq.Group;
	// The group's DEFLATE decoder (one window for the whole log), built on the first frame.
	#decoder?: Decoder;

	constructor(track: Moq.Track, config: ConsumerConfig = {}) {
		this.#track = track;
		this.#decompress = config.compression ?? false;
	}

	/** Get the next record, or `undefined` once the track ends. */
	async next(): Promise<T | undefined> {
		for (;;) {
			if (!this.#group) {
				this.#group = await this.#track.nextGroupOrdered();
				if (!this.#group) return undefined;
				this.#decoder = this.#decompress ? new Decoder() : undefined;
			}

			const frame = await this.#group.readFrame();
			if (frame === undefined) {
				// The group is finished; the log rides just this one, so the stream ends.
				this.#group = undefined;
				this.#decoder = undefined;
				continue;
			}

			const plain = this.#decoder ? this.#decoder.frame(frame) : frame;
			return JSON.parse(new TextDecoder().decode(plain));
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
