export type Icons = "network" | "video" | "audio" | "buffer";

export interface HandlerContext {
	setDisplayData: (data: string) => void;
	setFps?: (fps: number | null) => void;
}

export interface Signal<T> {
	peek(): T | undefined;
	subscribe?(callback: () => void): () => void;
}

export interface AudioSource {
	active?: Signal<string>;
	config?: Signal<AudioConfig>;
	bitrate?: Signal<number>;
}

export interface AudioConfig {
	sampleRate?: number;
	numberOfChannels?: number;
	bitrate?: number;
	codec?: string;
}

export interface VideoSource {
	display?: Signal<{ width: number; height: number }>;
	fps?: Signal<number>;
	syncStatus?: Signal<{ state: "ready" | "wait"; bufferDuration?: number }>;
	bufferStatus?: Signal<{ state: "empty" | "filled" }>;
	latency?: Signal<number>;
}

export interface HandlerProps {
	audio?: AudioSource;
	video?: VideoSource;
}

export interface IStatsHandler {
	setup(context: HandlerContext): void;
	cleanup(): void;
}

export type HandlerConstructor = new (props: HandlerProps) => IStatsHandler;

export interface StreamContainer extends HTMLElement {
	active?: { peek: () => { broadcast?: HandlerProps } | undefined } | { peek: () => HandlerProps | undefined };
}

export interface BroadcastContainer {
	broadcast?: HandlerProps;
}