import * as Moq from "@moq/net";
import { Time } from "@moq/net";

export type { BufferedRange, BufferedRanges, Frame } from "./types.ts";

import { Duration } from "@moq/clock";
import type { AudioConfig, VideoConfig } from "../catalog/index.ts";
import * as Aac from "../util/aac.ts";
import * as Opus from "../util/opus.ts";
import type { Format as ContainerFormat } from "./format.ts";
import type { Frame } from "./types.ts";

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

	/**
	 * Return the endpoint an empty codec payload carries: an audio group's terminal marker, or an older
	 * publisher's duration or discontinuity marker.
	 */
	end(frame: Frame): Duration | undefined {
		return this.kind !== "data" && frame.payload.byteLength === 0 ? frame.timestamp : undefined;
	}

	/** Decode one legacy frame, including an empty-payload marker. On a data track an object with no bytes is an older publisher's discontinuity marker, so it decodes to no frame. */
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

// `timestamp` microseconds as a group start on `track`: its value in the track's timescale,
// rounded down as moq-net converts. Legacy frames are timed, so a track whose timescale is unknown
// or absent refuses it. Mirrors the Rust `moq_mux::start`.
function keyOf(track: Moq.Track.Producer, timestamp: number): bigint {
	const scale = track.timescale;
	if (!scale) throw new Moq.Error.TimestampMismatch();
	return BigInt(Time.floorAt(Time.Timestamp.fromMicros(timestamp), scale));
}

// Whether a group starting at `timestamp` microseconds starts past every group on `track`.
function startsPast(track: Moq.Track.Producer, timestamp: number): boolean {
	const largest = track.largest();
	return largest === undefined || keyOf(track, timestamp) > largest.group;
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
	/**
	 * Mirrors the Rust `container::Finished::abort`, and like it changes nothing a reader sees: every
	 * group finished, so readers still draining get the rest of the track and its clean end, not `err`.
	 */
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
	// The newest timestamp written in the current group, the furthest point it reached.
	#end?: number;
	// The current group start and the previous group start, which bounds its frames.
	#start?: number;
	#floor?: number;
	// Gap between consecutive timestamps, used to close the last group when no successor exists.
	#interval?: number;

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
		// A keyframe opens a group starting at its timestamp, which has to start after every group the
		// track holds. One that doesn't, such as parameter sets and the picture they precede written
		// as two keyframes at one timestamp, continues the open group instead. With none open it is
		// refused, before anything changes.
		const opens = keyframe && startsPast(this.#track, timestamp);
		if (keyframe && !opens && !this.#group) throw new Error("a keyframe must start after the previous group");
		if (opens) {
			const rewound = this.#previous !== undefined && timestamp < this.#previous;
			this.#close(rewound ? undefined : timestamp);
			if (rewound) this.#interval = undefined;
			// A track's first group says nothing came before it, so a reader can tell it is the first.
			if (this.#track.largest() === undefined) {
				const start = keyOf(this.#track, timestamp);
				if (start > 0n) this.#track.skipGroups(0n, start);
			}
			this.#group = this.#track.appendGroup(Time.Timestamp.fromMicros(timestamp));
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
	 * The open group ends whole at `endpoint`, where the content stops, else at the furthest frame
	 * written rather than where the frame cadence would put the next one: whatever resumes may land
	 * sooner than one frame later, and an end past it reads as a rewind. A lone frame reaches only its
	 * own start, so its group ends one unit of the track's timescale past it. A reader learns the
	 * media stops at that end once no group continues it, so the group before a pause does not read
	 * as live until whatever resumes it. It writes no group, and with none open does nothing. Throws
	 * if `endpoint` precedes the last video frame.
	 */
	discontinuity(endpoint?: Duration) {
		const end = endpoint === undefined ? undefined : micros(endpoint);
		// Nothing is measured across the break, so a missing end has no cadence to estimate from.
		// An explicit end keeps the cadence until #close validates it, in case it throws.
		if (end === undefined) this.#interval = undefined;
		const reached = end ?? this.#end;
		let whole: bigint | undefined;
		if (this.#group && reached !== undefined) {
			whole = keyOf(this.#track, reached);
			// A group's end has to pass its start.
			if (whole <= this.#group.start) whole = this.#group.start + 1n;
		}
		this.#close(end, whole);
		this.#interval = undefined;
	}

	// Flush and close the current group, whole at `whole`, else at the supplied or estimated end.
	#close(end?: number, whole?: bigint) {
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
		// A data track's items have no cadence to estimate an end from.
		const interval = this.#format.kind === "data" ? undefined : this.#interval;
		end ??= this.#end !== undefined && interval !== undefined ? this.#end + interval : undefined;
		// A presentation endpoint cannot bound the decode-order tail after reordering.
		const bound = this.#reordered ? undefined : end;
		// A group ends whole where its content stops, which tells a reader whether the next group
		// continues it.
		const at = whole ?? (bound === undefined ? undefined : keyOf(this.#track, bound));
		if (at !== undefined && at > this.#group.start) this.#group.finishAt(at);
		else this.#group.finish();
		this.#group = undefined;
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

	/**
	 * Abort the current group and the track with `err`, which readers see in place of a clean end. A
	 * no-op after {@link finish}: the track is already complete.
	 */
	// fallow-ignore-next-line unused-class-member
	abort(err: Error) {
		this.#group?.abort(err);
		this.#group = undefined;
		this.#track.abort(err);
	}
}
