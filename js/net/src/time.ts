/**
 * {@link Timescale} and {@link Timestamp} for presentation timing.
 *
 * Durations and instants live in `@moq/clock`.
 *
 * @module
 */
import { Instant, now } from "@moq/clock";
import { toNumber } from "./util/bigint.ts";
import { checkSequence } from "./util/sequence.ts";

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

const MAX_SAFE_KEY = BigInt(Number.MAX_SAFE_INTEGER);

/**
 * The group key of an unscaled timestamp, or `undefined` when it is scaled.
 *
 * @internal
 */
export let keyOf: (timestamp: Timestamp) => bigint | undefined;

// The key the constructor is building an unscaled timestamp for, set only inside `unscaled`.
let building: bigint | undefined;

/**
 * A presentation timestamp: a raw value in a given {@link Timescale}, or a scale-less track's
 * group key.
 *
 * Mirrors the Rust `Timestamp`. Unlike a `@moq/clock` `Duration` it carries its
 * own scale, so a track can pick its units and conversions can't silently mix them up.
 *
 * A scaled value must be finite and in `[0, Number.MAX_SAFE_INTEGER]`. Fractions are allowed
 * ({@link Timestamp.now}, scale conversion). Encoding rounds down to an integer at the
 * track timescale, as the Rust `Timestamp::convert` does. Integer timestamps are exact through `2^53 - 1` units; this package
 * does not use BigInt for timestamps. {@link Timestamp.as} also refuses a converted
 * value that leaves that range, so a legal source timestamp cannot become an unencodable
 * wire timestamp after a scale change.
 *
 * A scale-less track has no time: each frame's timestamp is {@link Timestamp.unscaled}, whose
 * {@link value} is its group's key. Anything that needs a scale ({@link as}, {@link asMillis},
 * {@link asMicros}) throws a `RangeError` on one, and {@link isBefore} orders it against no
 * scaled timestamp.
 */
export class Timestamp {
	/**
	 * The raw value, in `scale` units, or the group key when unscaled.
	 *
	 * Reading a key past `Number.MAX_SAFE_INTEGER` throws a `RangeError`, since a number can't
	 * hold it exactly.
	 */
	readonly value!: number;
	/** Units per second the {@link value} is measured in, or `undefined` when unscaled. */
	readonly scale: Timescale | undefined;
	// The group key of an unscaled timestamp, exact at any u64.
	#key: bigint | undefined;

	/** Build a timestamp of `value` units at `scale`. */
	constructor(value: number, scale: Timescale) {
		const key = building;
		building = undefined;
		if (key !== undefined) {
			this.scale = undefined;
			this.#key = key;
			if (key <= MAX_SAFE_KEY) this.value = Number(key);
			// Built from any received key, so only reading it as a number can fail.
			else
				Object.defineProperty(this, "value", {
					enumerable: true,
					get: () => {
						throw new RangeError(`timestamp key outside the safe number range: ${key}`);
					},
				});
			return;
		}
		assertTimestampValue(value);
		this.value = value;
		this.scale = Timescale(scale);
	}

	/**
	 * A scale-less track's timestamp: `key` is its group's key (or a datagram's sequence), with no
	 * time. Any u64 key is accepted, including one past `Number.MAX_SAFE_INTEGER`.
	 */
	static unscaled(key: bigint | number): Timestamp {
		if (typeof key === "number") {
			if (!Number.isSafeInteger(key) || key < 0) throw new RangeError(`invalid timestamp key: ${key}`);
			key = BigInt(key);
		}
		building = checkSequence(key);
		// The constructor reads `building` first, so its arguments go unused.
		return new Timestamp(0, Timescale.SECOND);
	}

	static {
		keyOf = (timestamp) => timestamp.#key;
	}

	/** The installed clock's monotonic instant in milliseconds, not wall-clock time. */
	static now(): Timestamp {
		return new Timestamp(now().since(Instant.ZERO).asMillis(), Timescale.MILLI);
	}

	/** A timestamp of `ms` milliseconds. */
	static fromMillis(ms: number): Timestamp {
		return new Timestamp(ms, Timescale.MILLI);
	}

	/** A timestamp of `us` microseconds. */
	static fromMicros(us: number): Timestamp {
		return new Timestamp(us, Timescale.MICRO);
	}

	/**
	 * This timestamp's value re-expressed at `scale`; throws a `RangeError` if it is unscaled or
	 * the result is not in the safe range.
	 */
	as(scale: Timescale): number {
		const dest = Timescale(scale);
		const from = scaled(this);
		const converted = dest === from ? this.value : (this.value * dest) / from;
		assertTimestampValue(converted);
		return converted;
	}

	/** The value in milliseconds; throws a `RangeError` if it is unscaled. */
	asMillis(): number {
		return this.as(Timescale.MILLI);
	}

	/** The value in microseconds; throws a `RangeError` if it is unscaled. */
	asMicros(): number {
		return this.as(Timescale.MICRO);
	}

	/**
	 * Whether this presents before `other`, whatever their scales. Two unscaled timestamps order
	 * by key; a scaled and an unscaled one don't order, so neither is before the other.
	 */
	isBefore(other: Timestamp): boolean {
		const [a, b] = [this.#key, other.#key];
		if (a !== undefined || b !== undefined) return a !== undefined && b !== undefined && a < b;
		if (this.scale === other.scale) return this.value < other.value;
		// Cross-scale products can pass 2^53, so they are compared exactly in BigInt, as Rust does in u128.
		const [ma, ea] = binary(this.value);
		const [mb, eb] = binary(other.value);
		const e = Math.min(ea, eb);
		return (ma * BigInt(scaled(other))) << BigInt(ea - e) < (mb * BigInt(scaled(this))) << BigInt(eb - e);
	}
}

/**
 * A received frame's timestamp: the one its wire carried, or on a scale-less track, which
 * carries none, {@link Timestamp.unscaled} at its group's `key` (a datagram's sequence).
 * Mirrors Rust's `Timestamp::received`.
 *
 * @internal
 */
export function received(timestamp: Timestamp | undefined, key: bigint): Timestamp {
	return timestamp ?? Timestamp.unscaled(key);
}

// `timestamp`'s scale, or a `RangeError` when unscaled: a group key has no time to convert.
function scaled(timestamp: Timestamp): Timescale {
	if (timestamp.scale === undefined) throw new RangeError("an unscaled timestamp has no time");
	return timestamp.scale;
}

/**
 * `timestamp`'s value at `scale`, rounded down exactly, as Rust's integer `Timestamp::convert`.
 *
 * A double is a dyadic rational, so the conversion runs in BigInt with no rounding before the
 * floor: an aligned value keeps it (4611686021000000 ns is 4611686021 ms, where `as` gives
 * 4611686020.999999), and one instant gives one result whatever scale it came from. Throws a
 * `RangeError` past the safe integer range, or for an unscaled timestamp.
 *
 * @internal
 */
export function floorAt(timestamp: Timestamp, scale: Timescale): number {
	const from = scaled(timestamp);
	if (from === scale) return Math.floor(timestamp.value);
	const [mantissa, exponent] = binary(timestamp.value);
	return toNumber((mantissa * BigInt(scale)) / (BigInt(from) << BigInt(-exponent)));
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
