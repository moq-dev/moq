# `@moq/clock`

```ts
export class Elapsed extends Error {
	constructor();
}

export interface Installed {
	close(): void;
}

export const Instant: ((value: number) => Instant) & {
	add: (at: Instant, duration: Nano) => Instant;
	durationSince: (at: Instant, earlier: Instant) => Nano;
	sub: (at: Instant, duration: Nano) => Instant;
	zero: Instant;
};

export type Instant = number & {
	readonly _brand: "clock-instant";
};

export class Interval {
	constructor(period: Nano);
	close(): void;
	period(): Nano;
	setMissedTickBehavior(behavior: MissedTickBehavior): void;
	tick(): Promise<Instant>;
}

export class Manual {
	constructor();
	static readonly WALL_START: Wall;
	advance(duration: Nano): Promise<void>;
	drive<T>(work: PromiseLike<T>): Promise<T>;
	fire(): Promise<Instant>;
	install(): Installed;
	jump(duration: Nano): void;
	now(): Instant;
	stepWall(wall: Wall): void;
	wall(): Wall;
}

export const Micro: ((value: number) => Micro) & {
	fromMilli: (value: Milli) => Micro;
	fromNano: (value: Nano) => Micro;
	fromSecond: (value: Second) => Micro;
	toMilli: (value: Micro) => Milli;
	toNano: (value: Micro) => Nano;
	toSecond: (value: Micro) => Second;
	zero: Micro;
};

export type Micro = number & {
	readonly _brand: "clock-micro";
};

export const Milli: ((value: number) => Milli) & {
	fromMicro: (value: Micro) => Milli;
	fromNano: (value: Nano) => Milli;
	fromSecond: (value: Second) => Milli;
	toMicro: (value: Milli) => Micro;
	toNano: (value: Milli) => Nano;
	toSecond: (value: Milli) => Second;
	zero: Milli;
};

export type Milli = number & {
	readonly _brand: "clock-milli";
};

export const MissedTickBehavior: {
	readonly Burst: "burst";
	readonly Delay: "delay";
	readonly Skip: "skip";
};

export type MissedTickBehavior = typeof MissedTickBehavior[keyof typeof MissedTickBehavior];

export const Nano: ((value: number) => Nano) & {
	fromMicro: (value: Micro) => Nano;
	fromMilli: (value: Milli) => Nano;
	fromSecond: (value: Second) => Nano;
	toMicro: (value: Nano) => Micro;
	toMilli: (value: Nano) => Milli;
	toSecond: (value: Nano) => Second;
	zero: Nano;
};

export type Nano = number & {
	readonly _brand: "clock-nano";
};

export const Second: ((value: number) => Second) & {
	fromMicro: (value: Micro) => Second;
	fromMilli: (value: Milli) => Second;
	fromNano: (value: Nano) => Second;
	toMicro: (value: Second) => Micro;
	toMilli: (value: Second) => Milli;
	toNano: (value: Second) => Nano;
	zero: Second;
};

export type Second = number & {
	readonly _brand: "clock-second";
};

export const Wall: ((unixMillis: number) => Wall) & {
	UNIX_EPOCH: Wall;
	add: (at: Wall, duration: Nano) => Wall;
	durationSince: (at: Wall, earlier: Wall) => Nano;
	sub: (at: Wall, duration: Nano) => Wall;
};

export type Wall = number & {
	readonly _brand: "clock-wall";
};

export function interval(period: Nano): Interval;

export function now(): Instant;

export function origin(): Wall;

export function sleep(duration: Nano, options?: {
	ref?: boolean;
	signal?: AbortSignal;
}): Promise<void>;

export function sleepUntil(at: Instant, options?: {
	ref?: boolean;
	signal?: AbortSignal;
}): Promise<void>;

export function timeout<T>(duration: Nano, operation: PromiseLike<T>): Promise<T>;

export function wall(): Wall;

export function yieldNow(): Promise<void>;
```
