import type * as Moq from "@moq/net";
import type { Getter } from "@moq/signals";
import type { BufferedRanges } from "../backend";
import type { Source } from "./source";

// Video specific outputs that work regardless of the backend source (mse vs webcodecs).
export interface Backend {
	// The source of the video.
	source: Source;

	readonly output: {
		// The stats of the video.
		readonly stats: Getter<Stats | undefined>;

		// Whether the video is currently buffering
		readonly stalled: Getter<boolean>;

		// Buffered time ranges (for MSE backend).
		readonly buffered: Getter<BufferedRanges>;

		// The timestamp of the current frame.
		readonly timestamp: Getter<Moq.Time.Milli | undefined>;
	};
}

export interface Stats {
	frameCount: number;
	bytesReceived: number;
}
