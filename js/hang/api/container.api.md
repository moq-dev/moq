# `@moq/hang/container`

```ts
import { AudioConfig } from "@moq/hang/catalog";
import { Format as Format_1 } from "@moq/hang/container";
import { Net } from "@moq/hang";
import { Signals } from "@moq/hang";
import { VideoConfig } from "@moq/hang/catalog";

export interface BufferedRange {
	end: Net.Clock.Duration;
	start: Net.Clock.Duration;
}

export type BufferedRanges = BufferedRange[];

export class Consumer {
	constructor(track: Net.Track.Subscriber, props: ConsumerProps);
	readonly buffered: Signals.Getter<BufferedRanges>;
	close(): void;
	next(): Promise<Read | undefined>;
	readonly spread: Signals.Getter<Net.Clock.Duration>;
}

export interface ConsumerProps {
	delay?: Signals.GetterInit<Net.Track.Delay>;
	format: Format;
	predecessor?: Consumer;
	wait?: Signals.GetterInit<Net.Clock.Duration | undefined>;
	warmup?: Net.Clock.Duration;
}

export const DELAY_MAX: Net.Clock.Duration;

export interface Format {
	decode(payload: Uint8Array, timestamp: Net.Time.Timestamp): Frame[];
	end?(frame: Frame): Net.Clock.Duration | undefined;
	readonly kind?: "audio" | "video" | "data";
}

export interface Frame {
	duration?: Net.Clock.Duration;
	keyframe: boolean;
	payload: Uint8Array;
	timestamp: Net.Clock.Duration;
}

export namespace Legacy {
	export { BufferedRange };
	export { BufferedRanges };
	export interface Finished {
		abort(err: Error): void;
	}
	export class Format implements Format_1 {
		constructor(config: AudioConfig | VideoConfig | "audio" | "video" | "data");
		decode(frame: Uint8Array): Frame[];
		end(frame: Frame): Net.Clock.Duration | undefined;
		finishGroup(group: Net.Group.Producer, end?: Net.Clock.Duration): void;
		readonly kind: "audio" | "video" | "data";
	}
	export { Frame };
	export class Producer {
		constructor(track: Net.Track.Producer, format: Legacy.Format);
		abort(err: Error): void;
		discontinuity(endpoint?: Net.Clock.Duration): void;
		encode(data: Uint8Array | Legacy.Source, pts: Net.Clock.Duration, keyframe: boolean): void;
		finish(): Legacy.Finished;
	}
	export interface Source {
		byteLength: number;
		copyTo(buffer: Uint8Array): void;
	}
	export function encodeFrame(source: Uint8Array | Legacy.Source, timestamp: Net.Clock.Duration): Uint8Array;
	export function packetDuration(config: AudioConfig): ((payload: Uint8Array) => Net.Clock.Duration | undefined) | undefined;
}

export * as Loc from "@moq/loc";

export const MAX_AGE: Net.Clock.Duration;

export type Read = {
	frame: ReceivedFrame;
	group: bigint;
	kind: "frame";
} | {
	duration: Net.Clock.Duration;
	kind: "gap";
	timestamp: Net.Clock.Duration;
} | {
	end: Net.Clock.Duration;
	group: bigint;
	kind: "end";
};

export interface ReceivedFrame extends Frame {
	arrival: Net.Clock.Instant;
}

export type TrackInfoOptions = {
	maxAge?: Net.Clock.Duration | null;
	priority: number;
};

export function mergeBufferedRanges(a: BufferedRanges, b: BufferedRanges): BufferedRanges;

export function trackInfo(options: TrackInfoOptions): Net.Track.InfoInit;
```
