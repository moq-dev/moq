import * as Moq from "@moq/net";
import { Time } from "@moq/net";

export type { BufferedRange, BufferedRanges, Frame } from "./types";

import type { AudioConfig, VideoConfig } from "../catalog";
import type { Recorder as TimelineRecorder } from "../timeline";
import type { Format as ContainerFormat } from "./format";
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

	/** Return the video-frame or audio-source endpoint for an empty codec payload. */
	end(frame: Frame): Time.Micro | undefined {
		return this.kind !== "data" && frame.payload.byteLength === 0 ? frame.timestamp : undefined;
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
	#reordered = false;
	#group?: Moq.Group.Producer;
	#timeline?: TimelineRecorder;
	// The newest timestamp written, reported to the timeline when the track closes: the last
	// group has no successor to bound it, so its segment would be published a group short.
	#end?: Time.Micro;
	// Exclusive presentation end of finished groups. A frame below this is refused.
	#liveEdge?: Time.Micro;
	// Gap between consecutive timestamps, used to close the last group when no successor exists.
	#interval?: Time.Micro;
	// A discontinuity's marker is the newest group, so another one would say nothing new.
	#marked = false;

	/** Wrap a track to publish legacy-container frames into it. */
	constructor(track: Moq.Track.Producer, format: Format, props: ProducerProps = {}) {
		this.#format = format;
		this.#track = track;
		this.#timeline = props.timeline;
	}

	/** Encode and append a frame; a keyframe starts a new group. Throws if the first frame is not a keyframe, or if the timestamp sits below the live edge earlier groups reached. */
	encode(data: Uint8Array | Source, timestamp: Time.Micro, keyframe: boolean) {
		this.#marked = false;
		if (keyframe) {
			const rewound = this.#previous !== undefined && timestamp < this.#previous;
			this.#close(rewound ? undefined : timestamp);
			if (rewound) this.#interval = undefined;
			this.#refuse(timestamp);
			this.#group = this.#track.appendGroup();
			// Report the group the moment it opens: its start is this keyframe's timestamp.
			this.#timeline?.record(this.#group.sequence, timestamp, true);
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		} else {
			this.#refuse(timestamp);
		}

		this.#group?.writeFrame({
			payload: encodeFrame(data, timestamp),
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});

		this.#reordered ||= this.#previous !== undefined && timestamp < this.#previous;
		if (this.#previous !== undefined && timestamp > this.#previous) {
			const delta = (timestamp - this.#previous) as Time.Micro;
			this.#interval = delta;
		}
		this.#previous = timestamp;
		if (this.#end === undefined || timestamp > this.#end) this.#end = timestamp;
	}

	/**
	 * Close the current group and mark a break in the timeline: whatever comes next does not
	 * continue it. Call it when the timeline is about to jump, e.g. an encoder pausing for lack of
	 * demand or switching source; the next keyframe already rolls the group over on its own.
	 *
	 * `end` is where the content stops, estimated from the frame cadence when omitted. After closing
	 * the group, this publishes a marker group of one empty frame at `end`, or at the live edge
	 * without one. Without the marker, a group's reach runs to its successor's first frame, so the
	 * group before a pause reads as live until whatever resumes it, and a subscriber joining
	 * mid-break is handed that stale media. The marker bounds it, and it is the latest group a
	 * joiner lands on. Data tracks only close the group, since an empty payload is data. No marker
	 * is written until a frame follows the last one. Throws if `end` precedes the last video frame.
	 */
	discontinuity(end?: Time.Micro) {
		this.#close(end);
		// Nothing is measured across the break.
		this.#interval = undefined;
		const timestamp = end ?? this.#liveEdge;
		if (this.#format.kind === "data" || this.#marked || timestamp === undefined) return;

		const group = this.#track.appendGroup();
		this.#timeline?.record(group.sequence, timestamp, false);
		this.#timeline?.end(timestamp);
		group.writeFrame({
			payload: encodeFrame(new Uint8Array(), timestamp),
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});
		group.close();
		this.#liveEdge = this.#liveEdge === undefined ? timestamp : (Math.max(this.#liveEdge, timestamp) as Time.Micro);
		this.#marked = true;
	}

	// Flush and close the current group at the supplied or estimated end timestamp.
	#close(end?: Time.Micro) {
		if (!this.#group) return;
		if (
			this.#format.kind === "video" &&
			!this.#reordered &&
			end !== undefined &&
			this.#previous !== undefined &&
			end < this.#previous
		) {
			throw new Error("video group endpoint precedes its last frame");
		}
		end ??=
			this.#end !== undefined && this.#interval !== undefined
				? ((this.#end + this.#interval) as Time.Micro)
				: undefined;
		// A presentation endpoint cannot bound the decode-order tail after reordering.
		this.#format.finishGroup(this.#group, this.#reordered ? undefined : end);
		const bound = end ?? this.#end;
		if (bound !== undefined) this.#timeline?.end(bound);
		this.#group.close();
		this.#group = undefined;
		if (this.#end !== undefined) {
			this.#liveEdge =
				this.#liveEdge === undefined ? this.#end : (Math.max(this.#liveEdge, this.#end) as Time.Micro);
		}
		this.#end = undefined;
		this.#previous = undefined;
		this.#reordered = false;
	}

	#refuse(timestamp: Time.Micro) {
		if (this.#liveEdge !== undefined && timestamp < this.#liveEdge) {
			throw new Error("frame timestamp is below the live edge");
		}
	}

	/** Close the track and current group, optionally with an error. */
	close(err?: Error) {
		if (!err) this.#close();
		this.#group?.close(err);
		this.#track.close(err);
	}
}
