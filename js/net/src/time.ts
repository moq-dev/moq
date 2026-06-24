/** A duration in nanoseconds, branded so it can't be mixed with other units. */
export type Nano = number & { readonly _brand: "nano" };

/** Constructors, conversions, and arithmetic for {@link Nano} values. */
// Calling `Nano(x)` brands a raw number as nanoseconds. The unit is the caller's assertion: no
// conversion happens, so reach for `fromMicro`/`fromMilli`/`fromSecond` when the source has a unit.
export const Nano = Object.assign((value: number): Nano => value as Nano, {
	zero: 0 as Nano,
	fromMicro: (us: Micro): Nano => (us * 1_000) as Nano,
	fromMilli: (ms: Milli): Nano => (ms * 1_000_000) as Nano,
	fromSecond: (s: Second): Nano => (s * 1_000_000_000) as Nano,
	toMicro: (ns: Nano): Micro => (ns / 1_000) as Micro,
	toMilli: (ns: Nano): Milli => (ns / 1_000_000) as Milli,
	toSecond: (ns: Nano): Second => (ns / 1_000_000_000) as Second,
	now: (): Nano => (performance.now() * 1_000_000) as Nano,
	add: (a: Nano, b: Nano): Nano => (a + b) as Nano,
	sub: (a: Nano, b: Nano): Nano => (a - b) as Nano,
	mul: (a: Nano, b: number): Nano => (a * b) as Nano,
	div: (a: Nano, b: number): Nano => (a / b) as Nano,
	max: (a: Nano, b: Nano): Nano => Math.max(a, b) as Nano,
	min: (a: Nano, b: Nano): Nano => Math.min(a, b) as Nano,
});

/** A duration in microseconds, branded so it can't be mixed with other units. */
export type Micro = number & { readonly _brand: "micro" };

/** Constructors, conversions, and arithmetic for {@link Micro} values. */
// Calling `Micro(x)` brands a raw number as microseconds. See the `Nano` note: this asserts the unit
// rather than converting, so use `fromNano`/`fromMilli`/`fromSecond` to convert from another unit.
export const Micro = Object.assign((value: number): Micro => value as Micro, {
	zero: 0 as Micro,
	fromNano: (ns: Nano): Micro => (ns / 1_000) as Micro,
	fromMilli: (ms: Milli): Micro => (ms * 1_000) as Micro,
	fromSecond: (s: Second): Micro => (s * 1_000_000) as Micro,
	toNano: (us: Micro): Nano => (us * 1_000) as Nano,
	toMilli: (us: Micro): Milli => (us / 1_000) as Milli,
	toSecond: (us: Micro): Second => (us / 1_000_000) as Second,
	now: (): Micro => (performance.now() * 1_000) as Micro,
	add: (a: Micro, b: Micro): Micro => (a + b) as Micro,
	sub: (a: Micro, b: Micro): Micro => (a - b) as Micro,
	mul: (a: Micro, b: number): Micro => (a * b) as Micro,
	div: (a: Micro, b: number): Micro => (a / b) as Micro,
	max: (a: Micro, b: Micro): Micro => Math.max(a, b) as Micro,
	min: (a: Micro, b: Micro): Micro => Math.min(a, b) as Micro,
});

/** A duration in milliseconds, branded so it can't be mixed with other units. */
export type Milli = number & { readonly _brand: "milli" };

/** Constructors, conversions, and arithmetic for {@link Milli} values. */
// Calling `Milli(x)` brands a raw number as milliseconds. See the `Nano` note: this asserts the unit
// rather than converting, so use `fromNano`/`fromMicro`/`fromSecond` to convert from another unit.
export const Milli = Object.assign((value: number): Milli => value as Milli, {
	zero: 0 as Milli,
	fromNano: (ns: Nano): Milli => (ns / 1_000_000) as Milli,
	fromMicro: (us: Micro): Milli => (us / 1_000) as Milli,
	fromSecond: (s: Second): Milli => (s * 1_000) as Milli,
	toNano: (ms: Milli): Nano => (ms * 1_000_000) as Nano,
	toMicro: (ms: Milli): Micro => (ms * 1_000) as Micro,
	toSecond: (ms: Milli): Second => (ms / 1_000) as Second,
	now: (): Milli => performance.now() as Milli,
	add: (a: Milli, b: Milli): Milli => (a + b) as Milli,
	sub: (a: Milli, b: Milli): Milli => (a - b) as Milli,
	mul: (a: Milli, b: number): Milli => (a * b) as Milli,
	div: (a: Milli, b: number): Milli => (a / b) as Milli,
	max: (a: Milli, b: Milli): Milli => Math.max(a, b) as Milli,
	min: (a: Milli, b: Milli): Milli => Math.min(a, b) as Milli,
});

/** Units per second for a {@link Timestamp}'s value, e.g. `1000` for milliseconds. */
export type Timescale = number & { readonly _brand: "timescale" };

/** Named timescales and a checked constructor (rejects non-positive / non-integer values). */
export const Timescale = Object.assign(
	(unitsPerSecond: number): Timescale => {
		if (!Number.isInteger(unitsPerSecond) || unitsPerSecond <= 0) {
			throw new Error(`invalid timescale: ${unitsPerSecond}`);
		}
		return unitsPerSecond as Timescale;
	},
	{
		/** One unit per second. */
		SECOND: 1 as Timescale,
		/** 1,000 units per second. */
		MILLI: 1_000 as Timescale,
		/** 1,000,000 units per second. */
		MICRO: 1_000_000 as Timescale,
		/** 1,000,000,000 units per second. */
		NANO: 1_000_000_000 as Timescale,
	},
);

/**
 * A presentation timestamp: a raw value in a given {@link Timescale}.
 *
 * Mirrors the Rust `Timestamp`. Unlike the bare `Milli`/`Micro` aliases it carries its
 * own scale, so a track can pick its units and conversions can't silently mix them up.
 */
export class Timestamp {
	/** The raw value, in `scale` units. */
	readonly value: number;
	/** Units per second the {@link value} is measured in. */
	readonly scale: Timescale;

	/** Build a timestamp of `value` units at `scale`. */
	constructor(value: number, scale: Timescale) {
		this.value = value;
		this.scale = scale;
	}

	/** Wall-clock now, in milliseconds (`performance.now()`). */
	static now(): Timestamp {
		return new Timestamp(performance.now(), Timescale.MILLI);
	}

	/** A timestamp of `ms` milliseconds. */
	static fromMillis(ms: number): Timestamp {
		return new Timestamp(ms, Timescale.MILLI);
	}

	/** A timestamp of `us` microseconds. */
	static fromMicros(us: number): Timestamp {
		return new Timestamp(us, Timescale.MICRO);
	}

	/** This timestamp's value re-expressed at `scale` (a raw number, not a new Timestamp). */
	as(scale: Timescale): number {
		return scale === this.scale ? this.value : (this.value * scale) / this.scale;
	}

	/** The value in milliseconds. */
	asMillis(): number {
		return this.as(Timescale.MILLI);
	}

	/** The value in microseconds. */
	asMicros(): number {
		return this.as(Timescale.MICRO);
	}
}

/** A duration in seconds, branded so it can't be mixed with other units. */
export type Second = number & { readonly _brand: "second" };

/** Constructors, conversions, and arithmetic for {@link Second} values. */
// Calling `Second(x)` brands a raw number as seconds. See the `Nano` note: this asserts the unit
// rather than converting, so use `fromNano`/`fromMicro`/`fromMilli` to convert from another unit.
export const Second = Object.assign((value: number): Second => value as Second, {
	zero: 0 as Second,
	fromNano: (ns: Nano): Second => (ns / 1_000_000_000) as Second,
	fromMicro: (us: Micro): Second => (us / 1_000_000) as Second,
	fromMilli: (ms: Milli): Second => (ms / 1_000) as Second,
	toNano: (s: Second): Nano => (s * 1_000_000_000) as Nano,
	toMicro: (s: Second): Micro => (s * 1_000_000) as Micro,
	toMilli: (s: Second): Milli => (s * 1_000) as Milli,
	now: (): Second => (performance.now() / 1_000) as Second,
	add: (a: Second, b: Second): Second => (a + b) as Second,
	sub: (a: Second, b: Second): Second => (a - b) as Second,
	mul: (a: Second, b: number): Second => (a * b) as Second,
	div: (a: Second, b: number): Second => (a / b) as Second,
	max: (a: Second, b: Second): Second => Math.max(a, b) as Second,
	min: (a: Second, b: Second): Second => Math.min(a, b) as Second,
});
