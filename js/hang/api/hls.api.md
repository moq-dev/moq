# `@moq/hang/hls`

```ts
import { Hls } from "@moq/hang/catalog";
import { Micro } from "@moq/clock";
import { Net } from "@moq/hang";
import { Signals } from "@moq/hang";
import { Window } from "@moq/json";

export interface Config {
	duration?: Micro;
	durationHint?: Micro;
}

export class Consumer implements AsyncIterable<Event> {
	[Symbol.asyncIterator](): AsyncIterator<Event>;
	close(): void;
	next(): Promise<Event | undefined>;
	static subscribe(broadcast: Net.Broadcast.Consumer, hls: Hls): Consumer;
}

export const DEFAULT_DURATION: Micro;

export type Entry = Omit<Record, "group" | "pts" | "duration"> & {
	duration?: Micro;
	group: bigint;
	pts: Micro;
};

export type Event = {
	push: {
		entry: Entry;
		index: number;
	};
} | {
	pop: Window.Span;
} | {
	skip: Window.Span;
};

export interface Record {
	duration?: number;
	group: number;
	pts: number;
	sequence: number;
}

export class Recorder {
	constructor(track: Net.Track.Producer, config?: Config);
	static readonly INFO: Net.Track.InfoInit;
	abort(err: Error): void;
	end(pts: Micro): void;
	finish(): void;
	frame(group: bigint, pts: Micro): void;
	readonly hls: Signals.Getter<Hls | undefined>;
	stop(group: bigint): void;
	readonly track: Net.Track.Producer;
}

export const SUFFIX: ".hls";

export class Segmenter {
	constructor(config?: Config);
	close(): void;
	get declared(): Micro | undefined;
	end(pts: Micro): void;
	frame(group: bigint, pts: Micro): void;
	stop(group: bigint): void;
	take(): Record[];
}

export function defaultName(track: string): string;
```
