import * as Moq from "@moq/net";
import { Time } from "@moq/net";

export type { BufferedRange, BufferedRanges, Frame } from "./types";

import type { AudioConfig, VideoConfig } from "../catalog";
import type { Format as ContainerFormat } from "./format";
import type { Recorder as TimelineRecorder } from "./timeline";
import type { Frame } from "./types";

/** The legacy hang container: a microsecond timestamp varint followed by the raw codec payload. */
export class Format implements ContainerFormat {
	/** Configure the format for the track's media kind. */
	readonly kind: "audio" | "video" | "data";

	/** Configure the format from a catalog entry or an explicit media kind. */
	constructor(config: AudioConfig | VideoConfig | "audio" | "video" | "data") {
		this.kind = typeof config === "string" ? config : "sampleRate" in config ? "audio" : "video";
	}

	/** Write the final video frame's end timestamp before the group closes. */
	finishGroup(group: Moq.Group.Producer, end?: Time.Micro) {
		if (this.kind !== "video" || end === undefined) return;
		group.writeFrame({ payload: encodeFrame(new Uint8Array(), end), timestamp: Time.Timestamp.fromMicros(end) });
	}

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
	#format: Format;
	#previous?: Time.Micro;
	#group?: Moq.Group.Producer;
	#timeline?: TimelineRecorder;
	// The newest timestamp written, reported to the timeline when the track closes: the last
	// group has no successor to bound it, so its segment would be published a group short.
	#end?: Time.Micro;
	// Gap between consecutive timestamps, used to close the last group when no successor exists.
	#interval?: Time.Micro;

	/** Wrap a track to publish legacy-container frames into it. */
	constructor(track: Moq.Track.Producer, format: Format, props: ProducerProps = {}) {
		this.#format = format;
		this.#track = track;
		this.#timeline = props.timeline;
	}

	/** Encode and append a frame; a keyframe starts a new group. Throws if the first frame is not a keyframe. */
	encode(data: Uint8Array | Source, timestamp: Time.Micro, keyframe: boolean) {
		if (keyframe) {
			this.cut(timestamp);
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

		if (this.#previous !== undefined && timestamp > this.#previous) {
			const delta = (timestamp - this.#previous) as Time.Micro;
			this.#interval = this.#interval === undefined ? delta : (Math.min(this.#interval, delta) as Time.Micro);
		}
		this.#previous = timestamp;
		if (this.#end === undefined || timestamp > this.#end) this.#end = timestamp;
	}

	/** Flush and close the current group at the supplied or estimated end timestamp. */
	cut(end?: Time.Micro) {
		if (!this.#group) return;
		end ??=
			this.#end !== undefined && this.#interval !== undefined
				? ((this.#end + this.#interval) as Time.Micro)
				: undefined;
		this.#format.finishGroup(this.#group, end);
		const bound = end ?? this.#end;
		if (bound !== undefined) this.#timeline?.end(bound);
		this.#group.close();
		this.#group = undefined;
		this.#end = undefined;
		this.#previous = undefined;
	}

	/** Close the track and current group, optionally with an error. */
	close(err?: Error) {
		if (!err) this.cut();
		this.#group?.close(err);
		this.#track.close(err);
	}
}
