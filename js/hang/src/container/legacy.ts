import * as Moq from "@moq/net";
import { Time } from "@moq/net";

export type { BufferedRange, BufferedRanges, Frame } from "./types";

import { Duration } from "@moq/clock";
import type { AudioConfig, VideoConfig } from "../catalog";
import * as Aac from "../util/aac";
import * as Opus from "../util/opus";
import type { Format as ContainerFormat } from "./format";
import type { Frame } from "./types";

/**
 * How long each packet of an audio track plays, as its codec states. Legacy frames carry no duration,
 * and the hang draft leaves it to the codec. Knowing it, a reader tells a group that ends where the next
 * one starts from one followed by lost audio, which sparse group sequences can't say. Mirrors Rust's
 * `legacy::Audio`. Undefined when a packet doesn't state its duration without decoding it.
 */
export function packetDuration(config: AudioConfig): ((payload: Uint8Array) => Duration | undefined) | undefined {
	const rate = config.sampleRate;
	if (!(rate > 0)) return undefined;
	if (config.codec === "opus") {
		return (payload) => {
			const samples = Opus.packetSamples(payload);
			return samples === undefined ? undefined : Duration.micros((samples * 1_000_000) / 48_000);
		};
	}
	// AAC-LC codes 1024 samples per packet. SBR and PS packets decode to twice that, at a rate the
	// catalog may or may not double, so they state nothing here.
	if (Aac.objectType(config.codec) === 2) {
		const duration = Duration.micros((1024 * 1_000_000) / rate);
		return (payload) => (payload.byteLength > 0 ? duration : undefined);
	}
	// Interleaved 32-bit samples, so the payload's length says.
	if (config.codec === "pcm" && config.numberOfChannels > 0) {
		const frameBytes = 4 * config.numberOfChannels;
		return (payload) => {
			if (payload.byteLength === 0 || payload.byteLength % frameBytes !== 0) return undefined;
			return Duration.micros(((payload.byteLength / frameBytes) * 1_000_000) / rate);
		};
	}
	return undefined;
}

/** The legacy hang container: a microsecond timestamp varint followed by the raw codec payload. */
export class Format implements ContainerFormat {
	/** Configure the format for the track's media kind. */
	readonly kind: "audio" | "video" | "data";

	// How long each audio packet plays, when built from a catalog entry whose codec states it.
	readonly #duration?: (payload: Uint8Array) => Duration | undefined;

	/** Configure the format from a catalog entry or an explicit media kind. Only an audio entry states its packets' durations. */
	constructor(config: AudioConfig | VideoConfig | "audio" | "video" | "data") {
		this.kind = typeof config === "string" ? config : "sampleRate" in config ? "audio" : "video";
		if (typeof config !== "string" && "sampleRate" in config) this.#duration = packetDuration(config);
	}

	/** Write the final video frame's end timestamp before the group closes. */
	finishGroup(group: Moq.Group.Producer, end?: Duration) {
		if (this.kind !== "video" || end === undefined) return;
		group.writeFrame({
			payload: encodeFrame(new Uint8Array(), end),
			timestamp: Time.Timestamp.fromMicros(micros(end)),
		});
	}

	/** Return the video-frame or audio-source endpoint for an empty codec payload. */
	end(frame: Frame): Duration | undefined {
		return this.kind !== "data" && frame.payload.byteLength === 0 ? frame.timestamp : undefined;
	}

	/** Decode one legacy frame, including an empty-payload duration marker. On a data track an object with no bytes is the discontinuity marker, so it decodes to no frame. */
	decode(frame: Uint8Array): Frame[] {
		if (this.kind === "data" && frame.byteLength === 0) return [];
		const [timestamp, data] = Moq.Varint.decode(frame);
		const duration = data.byteLength > 0 ? this.#duration?.(data) : undefined;
		return [{ payload: data, timestamp: Duration.micros(timestamp), keyframe: false, duration }];
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
export function encodeFrame(source: Uint8Array | Source, timestamp: Duration): Uint8Array {
	const timestampBytes = Moq.Varint.encode(micros(timestamp));
	const data = new Uint8Array(timestampBytes.byteLength + source.byteLength);
	data.set(timestampBytes, 0);

	if (source instanceof Uint8Array) {
		data.set(source, timestampBytes.byteLength);
	} else {
		source.copyTo(data.subarray(timestampBytes.byteLength));
	}

	return data;
}

// The wire carries whole microseconds, rounded down as moq-net's timestamps are.
function micros(timestamp: Duration): number {
	return Math.floor(timestamp.asMicros());
}

/**
 * A track its {@link Producer} finished: every frame is written and it takes no more.
 * {@link Producer.finish} returns it, mirroring the Rust `container::Finished`; the producer itself
 * still refuses a frame after the finish.
 */
export interface Finished {
	/** Abort the track with an error; see {@link Producer.abort}. */
	abort(err: Error): void;
}

/** Writes legacy-container frames into a MoQ track, starting a new group on each keyframe. */
export class Producer {
	#track: Moq.Track.Producer;
	#format: Format;
	// Timestamps in whole microseconds.
	#previous?: number;
	#reordered = false;
	#group?: Moq.Group.Producer;
	// The newest timestamp written in the current group, which estimates its end.
	#end?: number;
	// Furthest timestamp in finished groups, used only for discontinuity markers.
	#liveEdge?: number;
	// The current group start and the previous group start, which bounds its frames.
	#start?: number;
	#floor?: number;
	// Gap between consecutive timestamps, used to close the last group when no successor exists.
	#interval?: number;
	// A discontinuity's marker is the newest group, so another one would say nothing new.
	#marked = false;

	/** Wrap a track to publish legacy-container frames into it. */
	constructor(track: Moq.Track.Producer, format: Format) {
		this.#format = format;
		this.#track = track;
	}

	/** Encode and append a frame; a keyframe starts a new group. Throws if the first frame is not a keyframe, a group start goes backwards, or a frame is below the previous group start. */
	encode(data: Uint8Array | Source, pts: Duration, keyframe: boolean) {
		const timestamp = micros(pts);
		const floor = keyframe ? this.#start : this.#floor;
		if (floor !== undefined && timestamp < floor) {
			throw new Error("frame timestamp is below the previous group start");
		}
		this.#marked = false;
		if (keyframe) {
			const rewound = this.#previous !== undefined && timestamp < this.#previous;
			this.#close(rewound ? undefined : timestamp);
			if (rewound) this.#interval = undefined;
			this.#group = this.#track.appendGroup();
			this.#floor = this.#start;
			this.#start = timestamp;
		} else if (!this.#group) {
			throw new Error("must start with a keyframe");
		}

		this.#group?.writeFrame({
			payload: encodeFrame(data, pts),
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});

		this.#reordered ||= this.#previous !== undefined && timestamp < this.#previous;
		if (this.#previous !== undefined && timestamp > this.#previous) {
			const delta = timestamp - this.#previous;
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
	 * `end` is where the content stops. Without one the group closes with no end estimated from the
	 * frame cadence, since whatever resumes may land sooner than one frame later and an end past it
	 * reads as a rewind. After closing the group, this publishes a marker group of one empty frame at
	 * `end`, or at the live edge without one. Without the marker, a group's reach runs to its
	 * successor's first frame, so the group before a pause reads as live until whatever resumes it,
	 * and a subscriber joining mid-break is handed that stale media. The marker bounds it, and it is
	 * the latest group a joiner lands on. An empty payload is data on a data track, so its marker is one
	 * object with no bytes at all. No marker is written until a frame follows the last one. Throws if
	 * `end` precedes the last video frame.
	 */
	discontinuity(endpoint?: Duration) {
		const end = endpoint === undefined ? undefined : micros(endpoint);
		// Nothing is measured across the break, so a missing end has no cadence to estimate from.
		// An explicit end keeps the cadence until #close validates it, in case it throws.
		if (end === undefined) this.#interval = undefined;
		this.#close(end);
		this.#interval = undefined;
		const timestamp = end ?? this.#liveEdge;
		if (this.#marked || timestamp === undefined) return;

		const group = this.#track.appendGroup();
		group.writeFrame({
			payload:
				this.#format.kind === "data"
					? new Uint8Array()
					: encodeFrame(new Uint8Array(), Duration.micros(timestamp)),
			timestamp: Time.Timestamp.fromMicros(timestamp),
		});
		group.finish();
		this.#liveEdge = this.#liveEdge === undefined ? timestamp : Math.max(this.#liveEdge, timestamp);
		this.#marked = true;
	}

	// Flush and close the current group at the supplied or estimated end timestamp.
	#close(end?: number) {
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
		end ??= this.#end !== undefined && this.#interval !== undefined ? this.#end + this.#interval : undefined;
		// A presentation endpoint cannot bound the decode-order tail after reordering.
		this.#format.finishGroup(this.#group, this.#reordered || end === undefined ? undefined : Duration.micros(end));
		this.#group.finish();
		this.#group = undefined;
		if (this.#end !== undefined) {
			this.#liveEdge = this.#liveEdge === undefined ? this.#end : Math.max(this.#liveEdge, this.#end);
		}
		this.#end = undefined;
		this.#previous = undefined;
		this.#reordered = false;
	}

	/** Close the current group and finish the track, returning it as {@link Finished}. */
	finish(): Finished {
		this.#close();
		this.#track.finish();
		this.#track.close();
		return this;
	}

	/** Abort the current group and the track with `err`, which readers see in place of a clean end. */
	abort(err: Error) {
		this.#group?.abort(err);
		this.#group = undefined;
		this.#track.abort(err);
	}
}
