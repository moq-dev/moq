import { Duration } from "@moq/clock";
import { Time } from "@moq/net";
import type { AudioConfig, VideoConfig } from "../catalog/index.ts";
import type { Format as ContainerFormat } from "./format.ts";
import { packetDuration } from "./legacy.ts";
import type { Frame } from "./types.ts";

/**
 * The raw hang container: no container header, raw codec payload timed directly by the MoQ
 * transport timestamp (DTS == PTS). Requires a timed track.
 */
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

	/** Return the video-frame or audio-source endpoint for an empty codec payload. */
	end(frame: Frame): Duration | undefined {
		return this.kind !== "data" && frame.payload.byteLength === 0 ? frame.timestamp : undefined;
	}

	/** Decode one raw frame using its MoQ transport timestamp. Throws on an untimed track. */
	decode(frame: Uint8Array, timestamp: Time.Timestamp): Frame[] {
		// A plain error, so a reader fails loudly rather than reading a stream error as the track ending.
		if (timestamp.scale === undefined) throw new Error("raw container requires a timed track");
		// On a data track a zero-byte object is only ever the discontinuity marker (decodes to no
		// frames), so an empty payload has no encoding and one sharing its group is malformed.
		if (this.kind === "data" && frame.byteLength === 0) return [];
		const duration = frame.byteLength > 0 ? this.#duration?.(frame) : undefined;
		return [
			{
				payload: frame,
				timestamp: Duration.micros(timestamp.as(Time.Timescale.MICRO)),
				keyframe: false,
				duration,
			},
		];
	}
}
