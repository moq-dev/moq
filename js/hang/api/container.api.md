# `@moq/hang/container`

```ts
import { Audio } from "@moq/hang/catalog";
import { Format as Format_1 } from "@moq/hang/container";
import { Micro } from "@moq/clock";
import { Milli } from "@moq/clock";
import { Net } from "@moq/hang";
import { Signals } from "@moq/hang";
import { Video } from "@moq/hang/catalog";

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
	maxBuffer?: number;
	maxDelay?: Signals.GetterInit<Milli>;
	predecessor?: Consumer;
}

export const DEFAULT_MAX_BUFFER: number;

export interface Format {
	decode(payload: Uint8Array, timestamp: Net.Time.Timestamp | undefined): Frame[];
	end?(frame: Frame): Micro | undefined;
	readonly kind?: "audio" | "video" | "data";
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
	export class Format implements Format_1 {
		constructor(config: Audio.AudioConfig | Video.VideoConfig | "audio" | "video" | "data");
		decode(frame: Uint8Array): Frame[];
		end(frame: Frame): Micro | undefined;
		finishGroup(group: Net.Group.Producer, end?: Micro): void;
		readonly kind: "audio" | "video" | "data";
	}
	export { Frame };
	export class Producer {
		constructor(track: Net.Track.Producer, format: Legacy.Format);
		close(err?: Error): void;
		discontinuity(end?: Micro): void;
		encode(data: Uint8Array | Legacy.Source, timestamp: Micro, keyframe: boolean): void;
	}
	export interface Source {
		byteLength: number;
		copyTo(buffer: Uint8Array): void;
	}
	export function encodeFrame(source: Uint8Array | Legacy.Source, timestamp: Micro): Uint8Array;
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
