export type KnownStatsProviders = "network" | "video" | "audio" | "buffer";

import type * as Hang from "@moq/hang";

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
export type Signal<T> = Hang.Moq.Signals.Getter<T>;
export type AudioStats = Hang.Watch.Audio.Stats;
export type AudioSource = Hang.Watch.Audio.Backend;
export type AudioConfig = Hang.Catalog.AudioConfig;
export type VideoStats = Hang.Watch.Video.Stats;

// TODO use Hang.Watch.Backend instead?
export type ProviderProps = {
	audio: Hang.Watch.Audio.Backend;
	video: Hang.Watch.Video.Backend;
};
