export type KnownStatsProviders = "network" | "video" | "audio" | "buffer";

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

/**
 * Stream sync status with buffer information
 */
export interface SyncStatus {
	state: "ready" | "wait";
	bufferDuration?: number;
}

/**
 * Stream buffer fill status
 */
export interface BufferStatus {
	state: "empty" | "filled";
}

/**
 * Generic reactive signal interface for accessing stream data
 */
export interface Signal<T> {
	peek(): T | undefined;
	changed?(callback: (value: T | undefined) => void): () => void;
	subscribe?(callback: () => void): () => void;
}

/**
 * Audio stream statistics
 */
export type AudioStats = {
	bytesReceived: number;
};

/**
 * Audio stream source with reactive properties
 */
export interface AudioSource {
	source: {
		active: Signal<string>;
		config: Signal<AudioConfig>;
		stats: Signal<AudioStats>;
	};
}

/**
 * Audio stream configuration properties
 */
export interface AudioConfig {
	sampleRate: number;
	numberOfChannels: number;
	bitrate?: number;
	codec: string;
}

/**
 * Video stream statistics
 */
export type VideoStats = {
	frameCount: number;
	timestamp: number;
	bytesReceived: number;
};

/**
 * Video stream source with reactive properties
 */
export interface VideoSource {
	source: {
		display: Signal<VideoResolution>;
		bufferStatus: Signal<BufferStatus>;
		stats: Signal<VideoStats>;
	};
}

/**
 * Props passed to metric providers containing stream sources
 */
export interface ProviderProps {
	audio?: AudioSource;
	video?: VideoSource;
}
