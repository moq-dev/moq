export type KnownStatsProviders = "network" | "video" | "audio" | "buffer";

import type * as Catalog from "@moq/hang/catalog";
import type { Getter } from "@moq/signals";

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
export type AudioStats = unknown;
export type AudioSource = unknown;
export type AudioConfig = Catalog.AudioConfig;
export type VideoStats = unknown;

// Note: Stats are primarily used for watching, not publishing
export type ProviderProps = {
	audio: unknown;
	video: unknown;
};
