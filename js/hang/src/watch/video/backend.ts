import type { Getter } from "@moq/signals";
import type { BufferedRanges } from "../backend";
import type { Source } from "./source";

// Video specific signals that work regardless of the backend source (mse vs webcodecs).
export interface Backend {
	// The source of the video.
	source: Source;

	// The stats of the video.
	stats: Getter<Stats | undefined>;

	// Buffered time ranges (for MSE backend).
	buffered: Getter<BufferedRanges>;
}

export interface Stats {
	frameCount: number;
	timestamp: number;
	bytesReceived: number;
}
