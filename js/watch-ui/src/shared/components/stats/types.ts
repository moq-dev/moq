export type KnownStatsProviders = "network" | "video" | "audio" | "buffer";

import type * as Catalog from "@moq/hang/catalog";
import type { Getter } from "@moq/signals";
import type * as Watch from "@moq/watch";

/**
 * Context passed to providers for updating display data
 */
export interface ProviderContext {
	setDisplayData: (data: string) => void;
}

/**
 * Video resolution dimensions
 */
export interface VideoResolution {
	width: number;
	height: number;
}

// TODO Don't re-export these types?
export type Signal<T> = Getter<T>;
export type AudioStats = Watch.Audio.Stats;
export type AudioSource = Watch.Audio.Backend;
export type AudioConfig = Catalog.AudioConfig;
export type VideoStats = Watch.Video.Stats;

// TODO use Watch.Backend instead?
export type ProviderProps = {
	audio: Watch.Audio.Backend;
	video: Watch.Video.Backend;
};
