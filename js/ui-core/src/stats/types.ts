export type KnownStatsProviders = "network" | "video" | "audio" | "buffer";

/**
 * A value that can be synchronously read via peek().
 * Matches @moq/signals Getter interface structurally.
 */
interface Peekable<T> {
	peek(): T;
}

/**
 * Context passed to providers for updating display data
 */
export interface ProviderContext {
	setDisplayData: (data: string) => void;
}

/**
 * Structural interface for an audio backend, matching what stats providers need.
 */
export interface AudioBackend {
	source: {
		track: Peekable<unknown>;
		config: Peekable<{ sampleRate?: number; numberOfChannels?: number; codec?: string } | undefined>;
	};
	stats: Peekable<{ bytesReceived: number } | undefined>;
}

/**
 * Structural interface for a video backend, matching what stats providers need.
 */
export interface VideoBackend {
	source: {
		catalog: Peekable<{ display?: { width: number; height: number } } | undefined>;
	};
	stats: Peekable<{ frameCount: number; bytesReceived: number } | undefined>;
}

/**
 * Structural interface for an established connection. Bandwidth values
 * are bits per second; rtt is milliseconds. Each is undefined when the
 * underlying transport doesn't expose it.
 */
export interface Connection {
	rtt?: Peekable<number | undefined>;
	recvBandwidth?: Peekable<number | undefined>;
	sendBandwidth?: Peekable<number | undefined>;
}

export type ProviderProps = {
	audio: AudioBackend;
	video: VideoBackend;
	connection?: Peekable<Connection | undefined>;
};
