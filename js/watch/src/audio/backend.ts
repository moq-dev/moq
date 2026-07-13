import type { Getter } from "@moq/signals";
import type { BufferedRanges } from "../buffered";
import type { Source } from "./source";

// Audio specific outputs that work regardless of the backend source (mse vs webcodecs).
export interface Backend {
	// The source of the audio.
	source: Source;

	readonly out: {
		// The stats of the audio.
		readonly stats: Getter<Stats | undefined>;

		// Buffered time ranges (for MSE backend).
		readonly buffered: Getter<BufferedRanges>;

		// The AudioContext used for playback (WebCodecs backend only).
		readonly context: Getter<AudioContext | undefined>;
	};
}

export interface Stats {
	sampleCount: number;
	bytesReceived: number;
}
