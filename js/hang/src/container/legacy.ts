import type { Time } from "@moq/net";
import * as Moq from "@moq/net";

export type { BufferedRange, BufferedRanges, Frame } from "./types";

import type { Format as ContainerFormat } from "./format";
import type { Producer as TimelineProducer } from "./timeline";
import type { Frame } from "./types";

/** The legacy hang container: a microsecond timestamp varint followed by the raw codec payload. */
export class Format implements ContainerFormat {
	/** Decode one legacy frame into a single media frame, or none if it's a marker. */
	decode(frame: Uint8Array): Frame[] {
		const [timestamp, data] = Moq.Varint.decode(frame);

		// An empty payload is a marker, not a sample: it says content stops at this
		// timestamp, so there's nothing to decode. Yield no frames rather than hand an
		// empty chunk to a decoder -- a publisher emitting these must not break us.
		if (data.byteLength === 0) return [];

		return [{ data, timestamp: timestamp as Time.Micro, keyframe: false }];
	}
}

/** A byte source that can be copied into a buffer, e.g. a WebCodecs EncodedChunk. */
export interface Source {
	/** Number of bytes the source will copy. */
	byteLength: number;
	/** Copy the source bytes into the given buffer. */
	copyTo(buffer: Uint8Array): void;
}

/** Encode a frame as a timestamp varint followed by the payload bytes. */
export function encodeFrame(source: Uint8Array | Source, timestamp: Time.Micro): Uint8Array {
	const timestampBytes = Moq.Varint.encode(timestamp);
	const data = new Uint8Array(timestampBytes.byteLength + source.byteLength);
	data.set(timestampBytes, 0);

	if (source instanceof Uint8Array) {
		data.set(source, timestampBytes.byteLength);
	} else {
		source.copyTo(data.subarray(timestampBytes.byteLength));
	}

	return data;
}

/** Options for a legacy-container {@link Producer}. */
export interface ProducerProps {
	/**
	 * Record each group open (sequence + start timestamp) into this companion timeline track, so
	 * consumers can index the media without downloading it.
	 */
	timeline?: TimelineProducer;
}

/** Writes legacy-container frames into a MoQ track, starting a new group on each keyframe. */
export class Producer {
	#track: Moq.Track;
	#group?: Moq.Group;
	#timeline?: TimelineProducer;

	/** Wrap a track to publish legacy-container frames into it. */
	constructor(track: Moq.Track, props: ProducerProps = {}) {
		this.#track = track;
		this.#timeline = props.timeline;
	}

	/** Encode and append a frame; a keyframe starts a new group. Throws if the first frame is not a keyframe. */
	encode(data: Uint8Array | Source, timestamp: Time.Micro, keyframe: boolean) {
		if (keyframe) {
			this.#group?.close();
			this.#group = this.#track.appendGroup();
			// Index the group the moment it opens: its start is this keyframe's timestamp.
			this.#timeline?.record(this.#group.sequence, timestamp);
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		}

		this.#group?.writeFrame(encodeFrame(data, timestamp));
	}

	/** Close the track and current group, optionally with an error. */
	close(err?: Error) {
		this.#track.close(err);
		this.#group?.close();
	}
}
