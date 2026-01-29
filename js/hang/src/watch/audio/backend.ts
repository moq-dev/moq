import type { Getter, Signal } from "@moq/signals";
import type { BufferedRanges } from "../backend";
import type { Source } from "./source";

// Audio specific signals that work regardless of the backend source (mse vs webcodecs).
export interface Backend {
	// The source of the audio.
	source: Source;

	// The volume of the audio, between 0 and 1.
	volume: Signal<number>;

	// Whether the audio is muted.
	muted: Signal<boolean>;

	// The stats of the audio.
	stats: Getter<Stats | undefined>;

	// Buffered time ranges (for MSE backend).
	buffered: Getter<BufferedRanges>;
}

export interface Stats {
	sampleCount: number;
	bytesReceived: number;
}
