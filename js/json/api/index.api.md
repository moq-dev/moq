# `@moq/json`

```ts
import { Time } from "@moq/net";
import { Timed } from "@moq/net";
import { Track } from "@moq/net";
import { ZodMiniType } from "@zod/mini";

export interface Diff {
	forcedSnapshot: boolean;
	patch: unknown;
}

export class DuplicateKey extends Error {
	constructor();
}

export class MissingSnapshot extends Error {
	constructor();
}

export namespace Snapshot {
	export interface Config<T> {
		deltas?: boolean;
		initial?: T;
		maxGroupBytes?: number;
		schema?: ZodMiniType<T>;
	}
	export namespace Consumer {
		export type Config<T> = Pick<Snapshot.Config<T>, "schema"> & {
			track: Track.Subscriber;
		};
	}
	export class Consumer<T> {
		constructor(config: Snapshot.Consumer.Config<T>);
		[Symbol.asyncIterator](): AsyncIterator<Timed<T>>;
		latest(): Promise<Timed<T> | undefined>;
		next(): Promise<Timed<T> | undefined>;
	}
	export class Decoder<T> {
		constructor(config?: Pick<Snapshot.Config<T>, "schema">);
		decode(): T | undefined;
		delta(payload: Uint8Array): void;
		snapshot(payload: Uint8Array): void;
		get value(): unknown;
	}
	export interface Encoded {
		keyframe: boolean;
		payload: Uint8Array;
	}
	export class Encoder<T> {
		constructor(config?: Snapshot.Config<T>);
		reset(): void;
		snapshot(): Snapshot.Pending | undefined;
		update(value: T): Snapshot.Pending | undefined;
		get value(): T | undefined;
	}
	export { MissingSnapshot };
	export interface Pending extends Snapshot.Encoded {
		commit(): void;
	}
	export namespace Producer {
		export type Config<T> = Snapshot.Config<T> & {
			track: Track.Producer;
		};
	}
	export class Producer<T> {
		constructor(config: Snapshot.Producer.Config<T>);
		cut(): void;
		finish(): void;
		mutate(fn: (value: T) => void, at?: Time.Timestamp): void;
		update({ ... }: Timed<T>): void;
	}
}

export namespace Stream {
	export interface Config<T = unknown> {
		schema?: ZodMiniType<T>;
	}
	export namespace Consumer {
		export type Config<T> = Stream.Config<T> & {
			track: Track.Subscriber;
		};
	}
	export class Consumer<T> {
		constructor(config: Stream.Consumer.Config<T>);
		[Symbol.asyncIterator](): AsyncIterator<Timed<T>>;
		next(): Promise<Timed<T> | undefined>;
	}
	export class Decoder<T> {
		constructor(config?: Stream.Config<T>);
		decode(payload: Uint8Array): T;
	}
	export class Encoder<T> {
		constructor(config?: Stream.Config<T>);
		encode(value: T): Stream.Pending;
		reset(): void;
	}
	export interface Pending {
		commit(): void;
		payload: Uint8Array;
	}
	export namespace Producer {
		export type Config<T> = Stream.Config<T> & {
			track: Track.Producer;
		};
	}
	export class Producer<T> {
		constructor(config: Stream.Producer.Config<T>);
		append({ ... }: Timed<T>): void;
		finish(): void;
	}
	export class Rolled extends Error {
		constructor();
	}
}

export namespace Window {
	export interface Config {
		checkpointRecords?: number;
		ops?: boolean;
	}
	export class Consumer<T> {
		constructor(config: Window.ConsumerConfig);
		[Symbol.asyncIterator](): AsyncIterator<Timed<Window.Event<T>>>;
		next(): Promise<Timed<Window.Event<T>> | undefined>;
		get offset(): number;
	}
	export type ConsumerConfig = {
		track: Track.Subscriber;
	};
	export class Decoder<T> {
		constructor();
		group(): Window.Group;
		next(): Window.Event<T> | undefined;
		get offset(): number;
	}
	export interface Encoded {
		keyframe: boolean;
		payload: Uint8Array;
	}
	export class Encoder<T> {
		constructor(config?: Window.Config);
		get end(): number;
		get offset(): number;
		pop(count: number): Window.Pending | undefined;
		push(value: T): Window.Pending;
		reset(): void;
		snapshot(): Window.Pending;
		get window(): T[];
	}
	export type Event<T> = {
		push: {
			index: number;
			value: T;
		};
	} | {
		pop: Window.Span;
	} | {
		skip: Window.Span;
	};
	export interface Group {
		decode(payload: Uint8Array): void;
	}
	export interface Pending extends Window.Encoded {
		commit(): void;
	}
	export class Producer<T> {
		constructor(config: Window.ProducerConfig);
		cut(): void;
		finish(): void;
		get offset(): number;
		pop(count: number, at?: Time.Timestamp): void;
		push({ ... }: Timed<T>): void;
		get window(): T[];
	}
	export type ProducerConfig = Window.Config & {
		track: Track.Producer;
	};
	export interface Span {
		end: number;
		start: number;
	}
}

export function deepEqual(a: unknown, b: unknown): boolean;

export function diff(oldVal: unknown, newVal: unknown): Diff;

export function merge(target: unknown, patch: unknown): unknown;
```
