import type { Getter, Signal } from "@moq/signals";
import type { BufferedRanges } from "../buffered";
import type { Source } from "./source";

/** Audio-specific signals that work regardless of the backend source (MSE vs WebCodecs). */
export interface Backend {
	/** The source of the audio. */
	source: Source;

	/** The volume of the audio, between 0 and 1. */
	volume: Signal<number>;

	/** Whether the audio is muted. */
	muted: Signal<boolean>;

	/** The stats of the audio. */
	stats: Getter<Stats | undefined>;

	/** Buffered time ranges for the MSE backend. */
	buffered: Getter<BufferedRanges>;

	/** The AudioContext used for WebCodecs playback. */
	context: Getter<AudioContext | undefined>;
}

/** Audio playback statistics. */
export interface Stats {
	/** Number of samples decoded. */
	sampleCount: number;
	/** Number of encoded bytes received. */
	bytesReceived: number;
}
