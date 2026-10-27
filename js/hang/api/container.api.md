# `@moq/hang/container`

```ts
import { $brand } from "@zod/mini";
import { Micro } from "@moq/clock";
import { Milli } from "@moq/clock";
import { Net } from "@moq/hang";
import { ReferenceEpoch } from "@moq/hang/catalog";
import { RelativeBroadcast } from "@moq/hang/catalog";
import { Signals } from "@moq/hang";

export interface BufferedRange {
	end: Milli;
	start: Milli;
}

export type BufferedRanges = BufferedRange[];

export class Consumer {
	constructor(track: Net.Track.Subscriber, props: ConsumerProps);
	readonly buffered: Signals.Getter<BufferedRanges>;
	close(): void;
	next(): Promise<Read | undefined>;
	readonly spread: Signals.Getter<Milli>;
}

export interface ConsumerProps {
	format: Format;
	maxDelay?: Signals.GetterInit<Milli>;
	predecessor?: Consumer;
}

export interface Format {
	decode(payload: Uint8Array, timestamp: Net.Time.Timestamp | undefined): Frame[];
	end?(frame: Frame): Micro | undefined;
	readonly kind?: "video" | "audio" | "data";
}

export interface Frame {
	duration?: Micro;
	keyframe: boolean;
	payload: Uint8Array;
	timestamp: Micro;
}

export namespace Legacy {
	export { BufferedRange };
	export { BufferedRanges };
	export class Format implements Format {
		constructor(config: {
			bitrate?: number & $brand<"u53">;
			broadcast?: RelativeBroadcast;
			codec: string;
			container: {
				kind: "unknown";
				raw: {
					[x: string]: unknown;
					kind: string;
				};
			} | {
				kind: "legacy";
			} | {
				init: string;
				kind: "cmaf";
			} | {
				kind: "loc";
			};
			delay?: number & $brand<"u53">;
			description?: string;
			enabled?: boolean;
			epoch?: ReferenceEpoch;
			hls?: {
				duration: number & $brand<"u53">;
				track: string;
			};
			jitter?: number & $brand<"u53">;
			label?: string;
			layout?: string;
			numberOfChannels: number & $brand<"u53">;
			sampleRate: number & $brand<"u53">;
			track?: string;
			warmup?: number & $brand<"u53">;
		} | {
			bitrate?: number & $brand<"u53">;
			broadcast?: RelativeBroadcast;
			codec: string;
			codedHeight?: number & $brand<"u53">;
			codedWidth?: number & $brand<"u53">;
			color?: {
				contentLight?: {
					maxCll: number;
					maxFall: number;
				};
				mastering?: {
					blue: {
						x: number;
						y: number;
					};
					green: {
						x: number;
						y: number;
					};
					maxLuminance: number;
					minLuminance: number;
					red: {
						x: number;
						y: number;
					};
					white: {
						x: number;
						y: number;
					};
				};
				matrix?: number & $brand<"u8">;
				primaries?: number & $brand<"u8">;
				range?: "full" | "limited";
				transfer?: number & $brand<"u8">;
			};
			container: {
				kind: "unknown";
				raw: {
					[x: string]: unknown;
					kind: string;
				};
			} | {
				kind: "legacy";
			} | {
				init: string;
				kind: "cmaf";
			} | {
				kind: "loc";
			};
			delay?: number & $brand<"u53">;
			description?: string;
			displayAspectHeight?: number & $brand<"u53">;
			displayAspectWidth?: number & $brand<"u53">;
			enabled?: boolean;
			epoch?: ReferenceEpoch;
			framerate?: number;
			hls?: {
				duration: number & $brand<"u53">;
				track: string;
			};
			interlaced?: boolean;
			jitter?: number & $brand<"u53">;
			label?: string;
			optimizeForLatency?: boolean;
			preference?: number;
			projection?: {
				kind: "unknown";
				raw: {
					[x: string]: unknown;
					kind: string;
				};
			} | {
				kind: "rectilinear";
			} | {
				fov?: number;
				kind: "equirectangular";
			};
			reorder?: number & $brand<"u8">;
			stereo?: string;
			track?: string;
			warmup?: number & $brand<"u53">;
		} | "video" | "audio" | "data");
		decode(frame: Uint8Array): Frame[];
		end(frame: Frame): Micro | undefined;
		finishGroup(group: Net.Group.Producer, end?: Micro): void;
		readonly kind: "video" | "audio" | "data";
	}
	export { Frame };
	export class Producer {
		constructor(track: Net.Track.Producer, format: Legacy.Format);
		close(err?: Error): void;
		discontinuity(end?: Micro): void;
		encode(data: Uint8Array<ArrayBufferLike> | Legacy.Source, timestamp: Micro, keyframe: boolean): void;
	}
	export interface Source {
		byteLength: number;
		copyTo(buffer: Uint8Array): void;
	}
	export function encodeFrame(source: Uint8Array<ArrayBufferLike> | Legacy.Source, timestamp: Micro): Uint8Array;
}

export * as Loc from "@moq/loc";

export type Read = {
	frame: ReceivedFrame;
	group: number;
	kind: "frame";
} | {
	duration: Micro;
	kind: "gap";
	timestamp: Micro;
} | {
	end: Micro;
	group: number;
	kind: "end";
};

export interface ReceivedFrame extends Frame {
	arrival: Milli;
}

export type TrackInfoOptions = {
	maxAge?: Milli;
	priority: number;
};

export function mergeBufferedRanges(a: BufferedRanges, b: BufferedRanges): BufferedRanges;

export function trackInfo(options: TrackInfoOptions): Partial<Net.Track.Info>;
```
