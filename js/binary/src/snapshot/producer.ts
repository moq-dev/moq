import { DEFAULT_MAX_FRAME_SIZE, Encoder as Flate } from "@moq/flate";
import * as Moq from "@moq/net";
import { Time } from "@moq/net";

import { type Compression, isDeflate } from "../compression.ts";

/** Codec options for a snapshot track. */
export interface Config {
	/**
	 * Compress each value as its own raw DEFLATE stream.
	 *
	 * A snapshot group holds a single self-contained value, so there is no window to share: each
	 * value is compressed alone. A {@link Consumer} reading the frames must set the same
	 * {@link compression}. Defaults to `"none"`.
	 */
	compression?: Compression;
}

/**
 * Publishes a binary value to a track, one value per group.
 *
 * Each {@link update} rolls a new group holding the whole value, so a consumer only ever needs the
 * newest group and older ones are dropped. For a log where every payload survives, use the `Stream`
 * module.
 */
export class Producer {
	#track: Moq.Track.Producer;
	#compress: boolean;

	/** Wrap a track to publish a binary value into it. */
	constructor(config: Producer.Config) {
		this.#track = config.track;
		this.#compress = isDeflate(config.compression);
	}

	/**
	 * Publish a new value, superseding the previous one.
	 *
	 * Unlike `@moq/json`, an identical value is republished rather than skipped: comparing two
	 * opaque blobs costs a full scan, and only the caller knows whether its bytes changed.
	 *
	 * `timestamp` is when the payload was captured, written as its frame timestamp. Defaults to now.
	 */
	update(payload: Uint8Array, timestamp: Time.Timestamp = Time.Timestamp.now()): void {
		// Consumers all decode with `@moq/flate`'s default cap, so publishing past it would advertise
		// a value that always fails to read. Rejected before anything is published; unlike a stream
		// this is not terminal, since the previous value still stands and the next update supersedes.
		if (this.#compress && payload.byteLength > DEFAULT_MAX_FRAME_SIZE) {
			throw new Error(`payload larger than the decoder's ${DEFAULT_MAX_FRAME_SIZE} byte limit`);
		}

		// One frame per group, so the window spans a single value and starts cold every time.
		const encoded = this.#compress ? new Flate().frame(payload) : payload;

		// Check before opening a group. `appendGroup` publishes immediately, so letting `writeFrame`
		// reject the frame would leave an empty newest group behind: a snapshot consumer jumps to the
		// newest, so the previous value would be lost even though this update threw.
		if (encoded.byteLength > Moq.Group.MAX_GROUP_CACHE_BYTES) throw new Moq.Error.FrameTooLarge();

		const group = this.#track.appendGroup();
		try {
			group.writeFrame({ payload: encoded, timestamp });
		} finally {
			// The group is already visible on the track, so leaving it open on a failed write would
			// strand a subscriber that advanced into it with nothing to read and no end.
			group.close();
		}
	}

	/** Finish the track. */
	finish(): void {
		this.#track.close();
	}
}

type Init = Config & { track: Moq.Track.Producer };

export namespace Producer {
	/** Snapshot producer options, including the destination track. */
	export type Config = Init;
}
