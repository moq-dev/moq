import type { Getter, Signal } from "@moq/signals";
import type * as Catalog from "../../catalog";

// Audio specific signals that work regardless of the backend source (mse vs webcodecs).
export interface Backend {
	// The catalog of the audio.
	catalog: Getter<Catalog.Audio | undefined>;

	// The volume of the audio, between 0 and 1.
	volume: Signal<number>;

	// Whether the audio is muted.
	muted: Signal<boolean>;

	// The desired rendition/bitrate of the audio.
	target: Signal<Target | undefined>;

	// The name of the active rendition.
	rendition: Signal<string | undefined>;

	// The config of the active rendition.
	config: Getter<Catalog.AudioConfig | undefined>;

	// The stats of the audio.
	stats: Getter<Stats | undefined>;
}

export interface Stats {
	sampleCount: number;
	bytesReceived: number;
}

export type Target = {
	// Optional manual override for the selected rendition name.
	name?: string;
};
