import type { Getter, Signal } from "@moq/signals";
import type * as Catalog from "../../catalog";
import type { BufferedRanges } from "../backend";

// Video specific signals that work regardless of the backend source (mse vs webcodecs).
export interface Backend {
	// The catalog of the video.
	catalog: Getter<Catalog.Video | undefined>;

	// The desired size/rendition/bitrate of the video.
	target: Signal<Target | undefined>;

	// The name of the active rendition.
	rendition: Getter<string | undefined>;

	// The stats of the video.
	stats: Getter<Stats | undefined>;

	// The config of the active rendition.
	config: Getter<Catalog.VideoConfig | undefined>;

	// Buffered time ranges (for MSE backend).
	buffered: Getter<BufferedRanges>;
}

export type Target = {
	// Optional manual override for the selected rendition name.
	name?: string;

	// The desired size of the video in pixels.
	pixels?: number;

	// TODO bitrate
};

export interface Stats {
	frameCount: number;
	timestamp: number;
	bytesReceived: number;
}
