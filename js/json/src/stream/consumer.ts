import { Stream as Flate } from "@moq/flate";
import type * as Moq from "@moq/net";
import type { Timed } from "@moq/net";
import { Decoder } from "./decoder.ts";
import type { Config as CodecConfig } from "./encoder.ts";

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
 * Consumes an ordered log of JSON records from a track, yielding every record in order with its
 * frame's timestamp.
 *
 * A {@link Decoder} that owns its track, reading one record per frame. The log is a single group,
 * which is what makes the mode lossless: rolling to a second group means the records that would
 * have completed the first are gone, so a {@link Producer} that cannot write ends the track
 * instead. A second group is therefore a broken publisher, and reading it would present a gap as a
 * continuous log, so it throws {@link Rolled} rather than yielding the remainder. When something
 * else already owns the track, use the {@link Decoder} directly.
 *
 * The failure does not wait for the first group to end: whatever has already arrived in it is
 * yielded, and the read then throws rather than blocking on a group a broken publisher may never
 * finish.
 */
export class Consumer<T> {
	#consumer: Flate.Consumer;
	#decoder: Decoder<T>;
	// Include JSON validation in the read, so a schema cannot reenter this consumer.
	#reading = false;

	constructor(config: Consumer.Config<T>) {
		this.#consumer = new Flate.Consumer(config);
		// The byte consumer owns decompression and the single-group contract.
		this.#decoder = new Decoder({ schema: config.schema });
	}

	/** Get the next record with its frame's timestamp, or `undefined` once the track ends. */
	next(): Promise<Timed<T> | undefined> {
		if (this.#reading) throw new Error("multiple calls to next not supported");
		this.#reading = true;
		return this.#read();
	}

	async #read(): Promise<Timed<T> | undefined> {
		try {
			let record: Timed<Uint8Array> | undefined;
			try {
				record = await this.#consumer.next();
			} catch (err) {
				// Keep the JSON error identity for callers catching a rolled log.
				if (err instanceof Flate.Rolled) throw new Rolled();
				throw err;
			}
			if (!record) return undefined;
			return { value: this.#decoder.decode(record.value), at: record.at };
		} finally {
			this.#reading = false;
		}
	}

	/** Iterate over every record in order, until the track ends. */
	async *[Symbol.asyncIterator](): AsyncIterator<Timed<T>> {
		for (;;) {
			const record = await this.next();
			if (record === undefined) return;
			yield record;
		}
	}
}

export namespace Consumer {
	/** Stream consumer options, including the source track. */
	export type Config<T> = CodecConfig<T> & { track: Moq.Track.Subscriber };
}
