# `@moq/watch`

```ts
export type Arrival = {
	arrival: Clock.Milli;
	kind: "frame";
	late: boolean;
	timestamp: Clock.Milli;
} | {
	duration: Clock.Milli;
	kind: "gap";
	timestamp: Clock.Milli;
};

export interface Arrivals {
	[Symbol.asyncIterator](): AsyncIterator<Arrival>;
	close(): void;
	next(): Promise<Arrival | undefined>;
}

export namespace Audio {
	export class Decoder {
		constructor(props: Audio.DecoderProps);
		arrivals(): Arrivals;
		close(): void;
		readonly in: Signals.Readonlys<Audio.DecoderInput>;
		readonly out: Signals.Readonlys<Audio.DecoderOutput>;
		reset(): void;
		readonly source: Audio.Source;
		static supported: (config: Hang.Catalog.Audio.AudioConfig) => Promise<boolean>;
		readonly sync: Sync;
	}
	export type DecoderInput = {
		enabled: Signals.Getter<boolean>;
	};
	export type DecoderOutput = {
		buffered: Signals.Signal<BufferedRanges>;
		context: Signals.Signal<AudioContext | undefined>;
		error: Signals.Signal<Error | undefined>;
		root: Signals.Signal<AudioNode | undefined>;
		sampleRate: Signals.Signal<number | undefined>;
		stalled: Signals.Signal<boolean>;
		stats: Signals.Signal<Audio.Stats | undefined>;
		timestamp: Signals.Signal<Clock.Milli | undefined>;
	};
	export type DecoderProps = Signals.Inputs<Audio.DecoderInput> & {
		source: Audio.Source;
		sync: Sync;
	};
	export class Emitter {
		constructor(props: Audio.EmitterProps);
		close(): void;
		readonly in: Signals.Readonlys<Audio.EmitterInput>;
		readonly out: Signals.Readonlys<Audio.EmitterOutput>;
		readonly source: Audio.Decoder;
	}
	export type EmitterInput = {
		muted: Signals.Getter<boolean>;
		paused: Signals.Getter<boolean>;
		volume: Signals.Getter<number>;
	};
	export type EmitterOutput = {
		enabled: Signals.Signal<boolean>;
	};
	export type EmitterProps = Signals.Inputs<Audio.EmitterInput> & {
		source: Audio.Decoder;
	};
	export class Source {
		constructor(props?: Signals.Inputs<Audio.SourceInput>);
		close(): void;
		readonly in: Signals.Readonlys<Audio.SourceInput>;
		readonly out: Signals.Readonlys<Audio.SourceOutput>;
	}
	export type SourceInput = {
		broadcast: Signals.Getter<Broadcast | undefined>;
		supported: Signals.Getter<Audio.Supported | undefined>;
		target: Signals.Getter<Audio.Target | undefined>;
	};
	export type SourceOutput = {
		available: Signals.Signal<Record<string, Hang.Catalog.Audio.AudioConfig>>;
		catalog: Signals.Signal<Hang.Catalog.Audio | undefined>;
		config: Signals.Signal<Hang.Catalog.Audio.AudioConfig | undefined>;
		jitter: Signals.Signal<Clock.Milli | undefined>;
		track: Signals.Signal<string | undefined>;
	};
	export interface Stats {
		bytesReceived: number;
		underruns: number;
	}
	export type Supported = (config: Hang.Catalog.Audio.AudioConfig) => Promise<boolean>;
	export type Target = {
		name?: string;
	};
}

export class Broadcast {
	constructor(props?: Signals.Inputs<BroadcastInput>);
	close(): void;
	contribution(effect: Signals.Effect, id: string): Broadcast;
	readonly in: Signals.Readonlys<BroadcastInput>;
	readonly out: Signals.Readonlys<BroadcastOutput>;
}

export type BroadcastInput = {
	catalog: Signals.Getter<Hang.Catalog.Root | undefined>;
	catalogFormat: Signals.Getter<CatalogFormat | undefined>;
	enabled: Signals.Getter<boolean>;
	follow: Signals.Getter<boolean>;
	name: Signals.Getter<Net.Path.Valid>;
	origin: Signals.Getter<Net.Origin.Table | undefined>;
};

export type BroadcastOutput = {
	active: Signals.Signal<Net.Broadcast.Consumer | undefined>;
	catalog: Signals.Signal<Hang.Catalog.Root | undefined>;
	error: Signals.Signal<Error | undefined>;
	status: Signals.Signal<"offline" | "loading" | "live" | "error" | "unsupported">;
};

export { BufferedRange } from "@moq/hang/container";

export { BufferedRanges } from "@moq/hang/container";

export const CATALOG_FORMATS: readonly ["hang", "msf", "manual"];

export type CatalogFormat = typeof CATALOG_FORMATS[number];

export * as Clock from "@moq/clock";

export type Delay = "instant" | "auto" | Clock.Milli;

export * as Hang from "@moq/hang";

export * as Net from "@moq/net";

export class Player {
	constructor(props?: PlayerProps);
	readonly audio: Audio.Decoder;
	readonly broadcast: Broadcast;
	close(): void;
	readonly emitter: Audio.Emitter;
	readonly in: Signals.Readonlys<Omit<PlayerInput, "video" | "audio" | "text">> & { readonly [K in "video" | "audio" | "text"]: Signals.Readonlys<PlayerInput[K]> };
	readonly renderer: Video.Renderer;
	reset(): void;
	readonly sync: Sync;
	readonly text: Text.Source;
	readonly textRenderer: Text.Renderer;
	readonly video: Video.Decoder;
}

export type PlayerInput = {
	audio: {
		muted: Signals.Getter<boolean>;
		target: Signals.Getter<Audio.Target | undefined>;
		volume: Signals.Getter<number>;
	};
	buffer: Signals.Getter<Clock.Milli>;
	catalog: Signals.Getter<Hang.Catalog.Root | undefined>;
	catalogFormat: Signals.Getter<CatalogFormat | undefined>;
	delay: Signals.Getter<Delay>;
	enabled: Signals.Getter<boolean>;
	follow: Signals.Getter<boolean>;
	name: Signals.Getter<Net.Path.Valid>;
	origin: Signals.Getter<Net.Origin.Table | undefined>;
	paused: Signals.Getter<boolean>;
	probe: Signals.Getter<Net.Connection.Probe | undefined>;
	text: {
		container: Signals.Getter<HTMLElement | undefined>;
		target: Signals.Getter<string | undefined>;
	};
	video: {
		backend: Signals.Getter<Video.Backend>;
		canvas: Signals.Getter<HTMLCanvasElement | undefined>;
		target: Signals.Getter<Video.Target | undefined>;
		visible: Signals.Getter<Video.Visible>;
	};
};

export type PlayerProps = Signals.Inputs<Omit<PlayerInput, "video" | "audio" | "text">> & { [K in "video" | "audio" | "text"]?: Signals.Inputs<PlayerInput[K]> };

export * as Signals from "@moq/signals";

export class Sync {
	constructor(props?: Signals.Inputs<SyncInput>);
	close(): void;
	readonly in: Signals.Readonlys<SyncInput>;
	now(): Clock.Milli | undefined;
	readonly out: Signals.Readonlys<SyncOutput>;
	reset(): void;
}

export type SyncInput = {
	buffer: Signals.Getter<Clock.Milli>;
	delay: Signals.Getter<Delay>;
};

export type SyncOutput = {
	buffered: Signals.Signal<boolean>;
	delay: Signals.Signal<Clock.Milli>;
	instant: Signals.Signal<boolean>;
	maxDelay: Signals.Signal<Clock.Milli>;
	reference: Signals.Signal<number | undefined>;
	timestamp: Signals.Signal<Clock.Milli | undefined>;
};

export namespace Text {
	export class Renderer {
		constructor(props: Text.RendererProps);
		close(): void;
		readonly in: Signals.Readonlys<Text.RendererInput>;
		readonly source: Text.Source;
		readonly sync: Sync;
	}
	export type RendererInput = {
		container: Signals.Getter<HTMLElement | undefined>;
		enabled: Signals.Getter<boolean>;
	};
	export type RendererProps = Signals.Inputs<Text.RendererInput> & {
		source: Text.Source;
		sync: Sync;
	};
	export class Source {
		constructor(props?: Signals.Inputs<Text.SourceInput>);
		close(): void;
		readonly in: Signals.Readonlys<Text.SourceInput>;
		readonly out: Signals.Readonlys<Text.SourceOutput>;
	}
	export type SourceInput = {
		broadcast: Signals.Getter<Broadcast | undefined>;
		target: Signals.Getter<string | undefined>;
	};
	export type SourceOutput = {
		available: Signals.Signal<Record<string, Hang.Catalog.TextConfig>>;
		catalog: Signals.Signal<Hang.Catalog.Text | undefined>;
		config: Signals.Signal<Hang.Catalog.TextConfig | undefined>;
		track: Signals.Signal<string | undefined>;
	};
	export function renderable(config: Hang.Catalog.TextConfig): boolean;
}

export namespace Video {
	export { Backend } from "@moq/video";
	export class Decoder {
		constructor(props: Video.DecoderProps);
		close(): void;
		readonly in: Signals.Readonlys<Video.DecoderInput>;
		readonly out: Signals.Readonlys<Video.DecoderOutput>;
		readonly source: Video.Source;
		static supported: (config: Hang.Catalog.Video.VideoConfig) => Promise<boolean>;
		readonly sync: Sync;
	}
	export type DecoderInput = {
		enabled: Signals.Getter<boolean>;
	};
	export type DecoderOutput = {
		buffered: Signals.Signal<BufferedRanges>;
		display: Signals.Signal<{
			height: number;
			width: number;
		} | undefined>;
		error: Signals.Signal<Error | undefined>;
		frame: Signals.Signal<VideoFrame | undefined>;
		interlaced: Signals.Signal<boolean>;
		jitter: Signals.Signal<Clock.Milli | undefined>;
		stalled: Signals.Signal<boolean>;
		stats: Signals.Signal<Video.Stats | undefined>;
		timestamp: Signals.Signal<Clock.Milli | undefined>;
	};
	export type DecoderProps = Signals.Inputs<Video.DecoderInput> & {
		source: Video.Source;
		sync: Sync;
	};
	export class Renderer {
		constructor(props: Video.RendererProps);
		close(): void;
		readonly decoder: Video.Decoder;
		readonly in: Signals.Readonlys<Video.RendererInput>;
		readonly out: Signals.Readonlys<{
			error: Signals.Getter<Error | undefined>;
			frame: Signals.Getter<VideoFrame | undefined>;
			timestamp: Signals.Getter<Clock.Milli | undefined>;
			visible: Signals.Getter<boolean>;
		}>;
	}
	export type RendererInput = {
		backend: Signals.Getter<Video.Backend>;
		canvas: Signals.Getter<HTMLCanvasElement | undefined>;
		visible: Signals.Getter<Video.Visible>;
	};
	export type RendererProps = Signals.Inputs<Video.RendererInput> & {
		decoder: Video.Decoder;
	};
	export class Source {
		constructor(props?: Signals.Inputs<Video.SourceInput>);
		close(): void;
		readonly in: Signals.Readonlys<Video.SourceInput>;
		readonly out: Signals.Readonlys<Video.SourceOutput>;
	}
	export type SourceInput = {
		broadcast: Signals.Getter<Broadcast | undefined>;
		probe: Signals.Getter<Net.Connection.Probe | undefined>;
		supported: Signals.Getter<Video.Supported | undefined>;
		target: Signals.Getter<Video.Target | undefined>;
	};
	export type SourceOutput = {
		available: Signals.Signal<Record<string, Hang.Catalog.Video.VideoConfig>>;
		catalog: Signals.Signal<Hang.Catalog.Video | undefined>;
		config: Signals.Signal<Hang.Catalog.Video.VideoConfig | undefined>;
		error: Signals.Signal<Error | undefined>;
		track: Signals.Signal<string | undefined>;
	};
	export interface Stats {
		bytesReceived: number;
		frameCount: number;
	}
	export type Supported = (config: Hang.Catalog.Video.VideoConfig) => Promise<boolean>;
	export type Target = {
		bitrate?: number;
		height?: number;
		name?: string;
		pixels?: number;
		width?: number;
	};
	export type Visible = "never" | "always" | string & {};
}

export function assets(url: string | URL): void;
```
