/**
 * Icon type for stats metrics
 */
export type Icons = "network" | "video" | "audio" | "buffer";

/**
 * Context passed to handlers for updating display data
 */
export interface HandlerContext {
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
	bitrate: number;
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
		syncStatus: Signal<SyncStatus>;
		bufferStatus: Signal<BufferStatus>;
		latency: Signal<number>;
		stats: Signal<VideoStats>;
	};
}

/**
 * Props passed to metric handlers containing stream sources
 */
export interface HandlerProps {
	audio?: AudioSource;
	video?: VideoSource;
}

/**
 * Interface for metric handler implementations
 */
export interface IStatsHandler {
	setup(context: HandlerContext): void;
	cleanup(): void;
}

/**
 * Constructor type for metric handler classes
 */
export type HandlerConstructor = new (props: HandlerProps) => IStatsHandler;
