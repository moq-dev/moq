import * as Moq from "@moq/net";
import { Time } from "@moq/net";

export type { BufferedRange, BufferedRanges, Frame } from "./types";

import type { Format as ContainerFormat } from "./format";
import type { Recorder as TimelineRecorder } from "./timeline";
import type { Frame } from "./types";

/** The legacy hang container: a microsecond timestamp varint followed by the raw codec payload. */
export class Format implements ContainerFormat {
	/** Return the exclusive end of the previous frame for an empty codec payload. */
	end(frame: Frame): Time.Micro | undefined {
		return frame.payload.byteLength === 0 ? frame.timestamp : undefined;
	}

	/** Decode one legacy frame, including an empty-payload duration marker. */
	decode(frame: Uint8Array): Frame[] {
		const [timestamp, data] = Moq.Varint.decode(frame);
		return [{ payload: data, timestamp: timestamp as Time.Micro, keyframe: false }];
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
	 * Report each group open (sequence + start timestamp) into the broadcast's timeline, so
	 * consumers can index the media without downloading it. Mint one via
	 * {@link Timeline.Producer.track}.
	 */
	timeline?: TimelineRecorder;
}

/** Writes legacy-container frames into a MoQ track, starting a new group on each keyframe. */
export class Producer {
	#track: Moq.Track.Producer;
	#group?: Moq.Group.Producer;
	#timeline?: TimelineRecorder;
	// The newest timestamp written, reported to the timeline when the track closes: the last
	// group has no successor to bound it, so its segment would be published a group short.
	#end?: Time.Micro;
	// Gap between consecutive timestamps, used to close the last group when no successor exists.
	#interval?: Time.Micro;

	/** Wrap a track to publish legacy-container frames into it. */
	constructor(track: Moq.Track.Producer, props: ProducerProps = {}) {
		this.#track = track;
		this.#timeline = props.timeline;
	}

	/** Encode and append a frame; a keyframe starts a new group. Throws if the first frame is not a keyframe. */
	encode(data: Uint8Array | Source, timestamp: Time.Micro, keyframe: boolean) {
		if (keyframe) {
			this.#writeDurationMarker(timestamp);
			this.#group?.close();
			this.#group = this.#track.appendGroup();
			// Report the group the moment it opens: its start is this keyframe's timestamp.
			this.#timeline?.record(this.#group.sequence, timestamp, true);
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		}

		this.#group?.writeFrame({
			payload: encodeFrame(data, timestamp),
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});

		if (this.#end !== undefined && timestamp > this.#end) {
			this.#interval = (timestamp - this.#end) as Time.Micro;
		}
		if (this.#end === undefined || timestamp > this.#end) this.#end = timestamp;
	}

	#writeDurationMarker(timestamp: Time.Micro) {
		if (!this.#group) return;
		this.#group.writeFrame({
			payload: encodeFrame(new Uint8Array(), timestamp),
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});
		this.#timeline?.end(timestamp);
	}

	/** Close the track and current group, optionally with an error. */
	close(err?: Error) {
		if (this.#end !== undefined && this.#interval !== undefined) {
			this.#writeDurationMarker((this.#end + this.#interval) as Time.Micro);
		} else if (this.#end !== undefined) {
			this.#timeline?.end(this.#end);
		}
		this.#track.close(err);
		this.#group?.close();
	}
}
