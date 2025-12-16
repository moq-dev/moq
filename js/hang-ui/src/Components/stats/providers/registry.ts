import type { KnownStatsProviders, ProviderProps } from "../types";
import { AudioProvider } from "./audio";
import type { BaseProvider } from "./base";
import { BufferProvider } from "./buffer";
import { NetworkProvider } from "./network";
import { VideoProvider } from "./video";

/**
 * Constructor type for metric provider classes
 */
export type ProviderConstructor = new (props: ProviderProps) => BaseProvider;

/**
 * Registry mapping metric types to their provider implementations
 */
export const providers: Record<KnownStatsProviders, ProviderConstructor> = {
	video: VideoProvider,
	audio: AudioProvider,
	buffer: BufferProvider,
	network: NetworkProvider,
};

/**
 * Get provider class for a metric type
 * @param statProvider - Metric type identifier
 * @returns Provider constructor or undefined if not found
 */
export function getStatsInformationProvider(statProvider: KnownStatsProviders): ProviderConstructor | undefined {
	return providers[statProvider];
}
