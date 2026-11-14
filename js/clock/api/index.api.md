# `@moq/clock`

```ts
export class Duration {
	static readonly ZERO: Duration;
	[Symbol.toPrimitive](hint: string): string;
	absDiff(other: Duration): Duration;
	add(other: Duration): Duration;
	asMicros(): number;
	asMillis(): number;
	asNanos(): number;
	asSecs(): number;
	checkedAdd(other: Duration): Duration | undefined;
	clamp(lo: Duration, hi: Duration): Duration;
	cmp(other: Duration): -1 | 0 | 1;
	div(divisor: number): Duration;
	divDuration(other: Duration): number;
	eq(other: Duration): boolean;
	ge(other: Duration): boolean;
	gt(other: Duration): boolean;
	isZero(): boolean;
	le(other: Duration): boolean;
	lt(other: Duration): boolean;
	max(other: Duration): Duration;
	static micros(value: number): Duration;
	static millis(value: number): Duration;
	min(other: Duration): Duration;
	mul(factor: number): Duration;
	static nanos(value: number): Duration;
	saturatingAdd(other: Duration): Duration;
	saturatingSub(other: Duration): Duration;
	static secs(value: number): Duration;
	sub(other: Duration): Duration;
	toJSON(): never;
	toString(): string;
	valueOf(): never;
}

export class Elapsed extends Error {
	constructor();
}

export interface Installed {
	close(): void;
}

export class Instant {
	static readonly ZERO: Instant;
	[Symbol.toPrimitive](hint: string): string;
	checkedAdd(duration: Duration): Instant | undefined;
	checkedSub(duration: Duration): Instant | undefined;
	cmp(other: Instant): -1 | 0 | 1;
	eq(other: Instant): boolean;
	ge(other: Instant): boolean;
	gt(other: Instant): boolean;
	le(other: Instant): boolean;
	lt(other: Instant): boolean;
	max(other: Instant): Instant;
	min(other: Instant): Instant;
	saturatingAdd(duration: Duration): Instant;
	saturatingSub(duration: Duration): Instant;
	since(earlier: Instant): Duration;
	toJSON(): never;
	toString(): string;
	valueOf(): never;
}

export class Interval {
	constructor(period: Duration);
	close(): void;
	period(): Duration;
	setMissedTickBehavior(behavior: MissedTickBehavior): void;
	tick(): Promise<Instant>;
}

export const MissedTickBehavior: {
	readonly Burst: "burst";
	readonly Delay: "delay";
	readonly Skip: "skip";
};

export type MissedTickBehavior = typeof MissedTickBehavior[keyof typeof MissedTickBehavior];

export class Wall {
	static readonly UNIX_EPOCH: Wall;
	[Symbol.toPrimitive](hint: string): string;
	add(duration: Duration): Wall;
	asUnixMillis(): number;
	cmp(other: Wall): -1 | 0 | 1;
	eq(other: Wall): boolean;
	ge(other: Wall): boolean;
	gt(other: Wall): boolean;
	le(other: Wall): boolean;
	lt(other: Wall): boolean;
	since(earlier: Wall): Duration;
	sub(duration: Duration): Wall;
	toJSON(): never;
	toString(): string;
	static unixMillis(millis: number): Wall;
	valueOf(): never;
}

export function interval(period: Duration): Interval;

export function now(): Instant;

export function origin(): Wall;

export function sleep(duration: Duration, options?: {
	ref?: boolean;
	signal?: AbortSignal;
}): Promise<void>;

export function sleepUntil(at: Instant, options?: {
	ref?: boolean;
	signal?: AbortSignal;
}): Promise<void>;

export function timeout<T>(duration: Duration, operation: PromiseLike<T>): Promise<T>;

export function timeoutSignal(duration: Duration, options?: {
	ref?: boolean;
	signal?: AbortSignal;
}): AbortSignal;

export function wall(): Wall;

export function yieldNow(): Promise<void>;
```
