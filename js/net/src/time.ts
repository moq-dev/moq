/**
 * {@link Timescale} and {@link Timestamp} for presentation timing.
 *
 * Duration units (`Nano`, `Micro`, `Milli`, `Second`) live in `@moq/clock`.
 *
 * @module
 */
import { now } from "@moq/clock";

/** Units per second for a {@link Timestamp}'s value, e.g. `1000` for milliseconds. */
export type Timescale = number & { readonly _brand: "timescale" };

/**
 * Named timescales and a checked constructor.
 *
 * Units per second must be a safe integer ≥ 1. JavaScript numbers are exact only
 * through `Number.MAX_SAFE_INTEGER` (`2^53 - 1`); the wire varint range goes
 * to `2^62 - 1`, which Rust accepts and this constructor refuses.
 */
export const Timescale = Object.assign(
	(unitsPerSecond: number): Timescale => {
		if (!Number.isSafeInteger(unitsPerSecond) || unitsPerSecond <= 0) {
			throw new RangeError(`invalid timescale: ${unitsPerSecond}`);
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

function assertTimestampValue(value: number): void {
	if (!Number.isFinite(value) || value < 0 || value > Number.MAX_SAFE_INTEGER) {
		throw new RangeError(`invalid timestamp: ${value}`);
	}
}

/**
 * A presentation timestamp: a raw value in a given {@link Timescale}.
 *
 * Mirrors the Rust `Timestamp`. Unlike `@moq/clock`'s bare duration units it carries its
 * own scale, so a track can pick its units and conversions can't silently mix them up.
 *
 * The value must be finite and in `[0, Number.MAX_SAFE_INTEGER]`. Fractions are allowed
 * ({@link Timestamp.now}, scale conversion). Encoding rounds to the nearest integer at the
 * track timescale. Integer timestamps are exact through `2^53 - 1` units; this package
 * does not use BigInt for timestamps. {@link Timestamp.as} also refuses a converted
 * value that leaves that range, so a legal source timestamp cannot become an unencodable
 * wire timestamp after a scale change.
 */
export class Timestamp {
	/** The raw value, in `scale` units. */
	readonly value: number;
	/** Units per second the {@link value} is measured in. */
	readonly scale: Timescale;

	/** Build a timestamp of `value` units at `scale`. */
	constructor(value: number, scale: Timescale) {
		assertTimestampValue(value);
		this.value = value;
		this.scale = Timescale(scale);
	}

	/** The installed clock's monotonic instant in milliseconds, not wall-clock time. */
	static now(): Timestamp {
		return new Timestamp(now() / 1_000_000, Timescale.MILLI);
	}

	/** A timestamp of `ms` milliseconds. */
	static fromMillis(ms: number): Timestamp {
		return new Timestamp(ms, Timescale.MILLI);
	}

	/** A timestamp of `us` microseconds. */
	static fromMicros(us: number): Timestamp {
		return new Timestamp(us, Timescale.MICRO);
	}

	/** This timestamp's value re-expressed at `scale`; throws if the result is not in the safe range. */
	as(scale: Timescale): number {
		const dest = Timescale(scale);
		const converted = dest === this.scale ? this.value : (this.value * dest) / this.scale;
		assertTimestampValue(converted);
		return converted;
	}

	/** The value in milliseconds. */
	asMillis(): number {
		return this.as(Timescale.MILLI);
	}

	/** The value in microseconds. */
	asMicros(): number {
		return this.as(Timescale.MICRO);
	}

	/** Whether this presents before `other`, whatever their scales. */
	isBefore(other: Timestamp): boolean {
		if (this.scale === other.scale) return this.value < other.value;
		// Cross-scale products can pass 2^53, so they are compared exactly in BigInt, as Rust does in u128.
		const [ma, ea] = binary(this.value);
		const [mb, eb] = binary(other.value);
		const e = Math.min(ea, eb);
		return (ma * BigInt(other.scale)) << BigInt(ea - e) < (mb * BigInt(this.scale)) << BigInt(eb - e);
	}
}

// A finite, non-negative `value` exactly as `mantissa * 2^exponent`. Doubling is exact, so
// a fraction becomes an integer within its binary digits.
function binary(value: number): [bigint, number] {
	let exponent = 0;
	while (!Number.isInteger(value)) {
		value *= 2;
		exponent--;
	}
	return [BigInt(value), exponent];
}
