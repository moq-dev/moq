import type { Time } from "@moq/net";
import * as Moq from "@moq/net";

export type { BufferedRange, BufferedRanges, Frame } from "./types";

import type { Format as ContainerFormat } from "./format";
import type { Frame } from "./types";

/** The legacy hang container: a microsecond timestamp varint followed by the raw codec payload. */
export class Format implements ContainerFormat {
	/** Decode one legacy frame into a single media frame. */
	decode(frame: Uint8Array): Frame[] {
		const [timestamp, data] = Moq.Varint.decode(frame);
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

/** Writes legacy-container frames into a MoQ track, starting a new group on each keyframe. */
export class Producer {
	#track: Moq.Track;
	#group?: Moq.Group;

	/** Wrap a track to publish legacy-container frames into it. */
	constructor(track: Moq.Track) {
		this.#track = track;
	}

	/** Encode and append a frame; a keyframe starts a new group. Throws if the first frame is not a keyframe. */
	encode(data: Uint8Array | Source, timestamp: Time.Micro, keyframe: boolean) {
		if (keyframe) {
			this.#group?.close();
			this.#group = this.#track.appendGroup();
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
