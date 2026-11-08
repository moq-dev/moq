function checked(value: number, what: string): number {
	if (!Number.isFinite(value) || value < 0) throw new RangeError(`invalid ${what}: ${value}`);
	return value;
}

function refuse(what: string): never {
	throw new TypeError(`a ${what} is not a number; read it with an explicit unit`);
}

// Values live under a symbol rather than a `#private` field so test matchers such as `toEqual`
// compare them by value; a private field is invisible to them, so every pair would match.
/** @internal */
export const VALUE: unique symbol = Symbol("clock value");
/** @internal */
export const NANOS: unique symbol = Symbol("clock nanos");

// Package-private bridges for backends that count in plain numbers.
/** @internal */
export let instantOf: (nanos: number) => Instant;
/** @internal */
export let instantNanos: (at: Instant) => number;
/** @internal */
export let wallOf: (unixMillis: number) => Wall;
/** @internal */
export let wallMillis: (at: Wall) => number;

const NANOS_PER_SEC = 1_000_000_000;

/**
 * A nonnegative span of time, mirroring Rust's `std::time::Duration`.
 *
 * Build one with a unit (`Duration.millis(5)`) and read it with one (`asMillis()`). It never
 * converts to a number implicitly: arithmetic, `<`, and `JSON.stringify` throw.
 *
 * Whole seconds and the whole nanoseconds past them are held apart, as Rust holds them, so a
 * whole number of any unit converts back exactly, such as microseconds since the Unix epoch.
 * A value finer than a nanosecond rounds to the nearest one.
 */
export class Duration {
	/** No time at all. */
	static readonly ZERO: Duration = new Duration(0, 0);

	/** @internal Whole seconds. */
	readonly [VALUE]: number;
	/** @internal Whole nanoseconds past the whole seconds, in `[0, 1e9)`. */
	readonly [NANOS]: number;
	declare private readonly kind: "duration";

	private constructor(secs: number, nanos: number) {
		this[VALUE] = secs;
		this[NANOS] = nanos;
	}

	// `value` units of which `perSec` make a second, split without rounding a whole count.
	static #unit(value: number, perSec: number): Duration {
		checked(value, "duration");
		let secs = Math.floor(value / perSec);
		let rest = value - secs * perSec;
		if (rest < 0) {
			secs -= 1;
			rest += perSec;
		} else if (rest >= perSec) {
			secs += 1;
			rest -= perSec;
		}
		let nanos = Math.round(rest * (NANOS_PER_SEC / perSec));
		if (nanos >= NANOS_PER_SEC) {
			secs += 1;
			nanos -= NANOS_PER_SEC;
		}
		if (!Number.isFinite(secs)) throw new RangeError(`invalid duration: ${value}`);
		return new Duration(secs, nanos);
	}

	// Whole seconds plus any nanoseconds, possibly out of range or fractional seconds, rounded to
	// whole nanoseconds as Rust's are, so a value's conversions agree with its ordering.
	static #carry(secs: number, nanos: number): Duration {
		let whole = Math.floor(secs);
		let rest = Math.round((secs - whole) * NANOS_PER_SEC + nanos);
		const carry = Math.floor(rest / NANOS_PER_SEC);
		whole += carry;
		rest -= carry * NANOS_PER_SEC;
		if (rest >= NANOS_PER_SEC) {
			whole += 1;
			rest -= NANOS_PER_SEC;
		} else if (rest < 0) {
			whole -= 1;
			rest += NANOS_PER_SEC;
		}
		if (!Number.isFinite(whole) || !Number.isFinite(rest) || whole < 0) {
			throw new RangeError(`invalid duration: ${secs}s + ${nanos}ns`);
		}
		return new Duration(whole, rest);
	}

	/** A duration of `value` nanoseconds, rounded to a whole one; throws a `RangeError` unless finite and nonnegative. */
	static nanos(value: number): Duration {
		return Duration.#unit(value, NANOS_PER_SEC);
	}

	/** A duration of `value` microseconds; throws a `RangeError` unless finite and nonnegative. */
	static micros(value: number): Duration {
		return Duration.#unit(value, 1_000_000);
	}

	/** A duration of `value` milliseconds; throws a `RangeError` unless finite and nonnegative. */
	static millis(value: number): Duration {
		return Duration.#unit(value, 1_000);
	}

	/** A duration of `value` seconds; throws a `RangeError` unless finite and nonnegative. */
	static secs(value: number): Duration {
		return Duration.#unit(value, 1);
	}

	/** The duration in nanoseconds; past 2^53 ns (about 104 days) it rounds. */
	asNanos(): number {
		return this[VALUE] * NANOS_PER_SEC + this[NANOS];
	}

	/** The duration in microseconds, possibly fractional. */
	asMicros(): number {
		return this[VALUE] * 1_000_000 + this[NANOS] / 1_000;
	}

	/** The duration in milliseconds, possibly fractional. */
	asMillis(): number {
		return this[VALUE] * 1_000 + this[NANOS] / 1_000_000;
	}

	/** The duration in seconds, possibly fractional. */
	asSecs(): number {
		return this[VALUE] + this[NANOS] / NANOS_PER_SEC;
	}

	/** Whether this is no time at all. */
	isZero(): boolean {
		return this[VALUE] === 0 && this[NANOS] === 0;
	}

	/** The sum of both durations. */
	add(other: Duration): Duration {
		return Duration.#carry(this[VALUE] + other[VALUE], this[NANOS] + other[NANOS]);
	}

	/** This duration minus `other`; throws a `RangeError` if `other` is longer. */
	sub(other: Duration): Duration {
		if (this.lt(other)) throw new RangeError(`invalid duration: ${this} - ${other}`);
		return Duration.#carry(this[VALUE] - other[VALUE], this[NANOS] - other[NANOS]);
	}

	/** This duration minus `other`, or zero if `other` is longer. */
	saturatingSub(other: Duration): Duration {
		return this.gt(other) ? this.sub(other) : Duration.ZERO;
	}

	/** This duration scaled by a finite, nonnegative factor. */
	mul(factor: number): Duration {
		checked(factor, "factor");
		return Duration.#carry(this[VALUE] * factor, this[NANOS] * factor);
	}

	/** This duration divided by a finite, positive divisor. */
	div(divisor: number): Duration {
		if (!(divisor > 0) || !Number.isFinite(divisor)) throw new RangeError(`invalid divisor: ${divisor}`);
		return Duration.#carry(this[VALUE] / divisor, this[NANOS] / divisor);
	}

	/** How many times `other` fits in this duration, possibly fractional; throws a `RangeError` if `other` is zero. */
	divDuration(other: Duration): number {
		if (other.isZero()) throw new RangeError("division by a zero duration");
		return this.asNanos() / other.asNanos();
	}

	/** The distance between both durations, whichever is longer. */
	absDiff(other: Duration): Duration {
		return this.ge(other) ? this.sub(other) : other.sub(this);
	}

	/** Whether both durations are the same length. */
	eq(other: Duration): boolean {
		return this[VALUE] === other[VALUE] && this[NANOS] === other[NANOS];
	}

	/** -1, 0, or 1 as this is less than, equal to, or greater than `other`, for sorting. */
	cmp(other: Duration): -1 | 0 | 1 {
		if (this[VALUE] !== other[VALUE]) return this[VALUE] < other[VALUE] ? -1 : 1;
		return this[NANOS] < other[NANOS] ? -1 : this[NANOS] > other[NANOS] ? 1 : 0;
	}

	/** Whether this duration is shorter than `other`. */
	lt(other: Duration): boolean {
		return this.cmp(other) < 0;
	}

	/** Whether this duration is at most `other`. */
	le(other: Duration): boolean {
		return this.cmp(other) <= 0;
	}

	/** Whether this duration is longer than `other`. */
	gt(other: Duration): boolean {
		return this.cmp(other) > 0;
	}

	/** Whether this duration is at least `other`. */
	ge(other: Duration): boolean {
		return this.cmp(other) >= 0;
	}

	/** The shorter of both durations. */
	min(other: Duration): Duration {
		return other.lt(this) ? other : this;
	}

	/** The longer of both durations. */
	max(other: Duration): Duration {
		return other.gt(this) ? other : this;
	}

	/** This duration limited to `[lo, hi]`. */
	clamp(lo: Duration, hi: Duration): Duration {
		return this.max(lo).min(hi);
	}

	/** The duration in milliseconds with a unit, such as `1.5ms`. */
	toString(): string {
		return `${this.asMillis()}ms`;
	}

	/** Refuses: a duration has no unitless number. */
	valueOf(): never {
		return refuse("Duration");
	}

	/** Refuses: serialize a number with an explicit unit instead. */
	toJSON(): never {
		return refuse("Duration");
	}

	[Symbol.toPrimitive](hint: string): string {
		if (hint === "string") return this.toString();
		return refuse("Duration");
	}
}

/**
 * A point on the installed clock's monotonic timeline, mirroring `kio::clock::Instant`.
 *
 * Instants are only comparable within one clock. The host clock's origin is the realm's
 * performance time origin, so a DOM timestamp `t` is `Instant.ZERO.add(Duration.millis(t))`.
 */
export class Instant {
	/** The clock's origin. */
	static readonly ZERO: Instant = new Instant(0);

	/** @internal Nanoseconds since the origin. */
	readonly [VALUE]: number;
	declare private readonly kind: "instant";

	private constructor(nanos: number) {
		this[VALUE] = nanos;
	}

	static {
		instantOf = (nanos) => new Instant(checked(nanos, "instant"));
		instantNanos = (at) => at[VALUE];
	}

	/** This instant moved forward by `duration`. */
	add(duration: Duration): Instant {
		return new Instant(checked(this[VALUE] + duration.asNanos(), "instant"));
	}

	/** This instant moved back by `duration`; throws a `RangeError` before the clock's origin. */
	sub(duration: Duration): Instant {
		return new Instant(checked(this[VALUE] - duration.asNanos(), "instant"));
	}

	/** The time from `earlier` to this instant, zero if `earlier` is later. */
	since(earlier: Instant): Duration {
		return this[VALUE] > earlier[VALUE] ? Duration.nanos(this[VALUE] - earlier[VALUE]) : Duration.ZERO;
	}

	/** Whether both are the same instant. */
	eq(other: Instant): boolean {
		return this[VALUE] === other[VALUE];
	}

	/** -1, 0, or 1 as this is less than, equal to, or greater than `other`, for sorting. */
	cmp(other: Instant): -1 | 0 | 1 {
		return this[VALUE] < other[VALUE] ? -1 : this[VALUE] > other[VALUE] ? 1 : 0;
	}

	/** Whether this instant is before `other`. */
	lt(other: Instant): boolean {
		return this[VALUE] < other[VALUE];
	}

	/** Whether this instant is at or before `other`. */
	le(other: Instant): boolean {
		return this[VALUE] <= other[VALUE];
	}

	/** Whether this instant is after `other`. */
	gt(other: Instant): boolean {
		return this[VALUE] > other[VALUE];
	}

	/** Whether this instant is at or after `other`. */
	ge(other: Instant): boolean {
		return this[VALUE] >= other[VALUE];
	}

	/** The earlier of both instants. */
	min(other: Instant): Instant {
		return other[VALUE] < this[VALUE] ? other : this;
	}

	/** The later of both instants. */
	max(other: Instant): Instant {
		return other[VALUE] > this[VALUE] ? other : this;
	}

	/** The time since the origin with a unit, such as `Instant(1.5ms)`. */
	toString(): string {
		return `Instant(${this[VALUE] / 1_000_000}ms)`;
	}

	/** Refuses: an instant has no unitless number. */
	valueOf(): never {
		return refuse("Instant");
	}

	/** Refuses: an instant is meaningless outside its clock. */
	toJSON(): never {
		return refuse("Instant");
	}

	[Symbol.toPrimitive](hint: string): string {
		if (hint === "string") return this.toString();
		return refuse("Instant");
	}
}

/**
 * A point in wall-clock time, separate from monotonic time, mirroring `kio::clock::Wall`.
 *
 * Convert Unix milliseconds exactly with `Wall.unixMillis(ms)` and back with `asUnixMillis()`.
 */
export class Wall {
	/** 1970-01-01T00:00:00Z. */
	static readonly UNIX_EPOCH: Wall = new Wall(0);

	/** @internal Unix milliseconds, since Unix nanoseconds pass 2^53. */
	readonly [VALUE]: number;
	declare private readonly kind: "wall";

	private constructor(millis: number) {
		this[VALUE] = millis;
	}

	static {
		wallOf = (millis) => new Wall(checked(millis, "wall time"));
		wallMillis = (at) => at[VALUE];
	}

	/** The time `millis` Unix milliseconds after the epoch; throws a `RangeError` before it or past the safe range. */
	static unixMillis(millis: number): Wall {
		return new Wall(checked(millis, "wall time"));
	}

	/** Unix milliseconds, exact: `since(Wall.UNIX_EPOCH)` goes through nanoseconds, which pass 2^53. */
	asUnixMillis(): number {
		return this[VALUE];
	}

	/** This time moved forward by `duration`. */
	add(duration: Duration): Wall {
		return new Wall(checked(this[VALUE] + duration.asMillis(), "wall time"));
	}

	/** This time moved back by `duration`; throws a `RangeError` before the Unix epoch. */
	sub(duration: Duration): Wall {
		return new Wall(checked(this[VALUE] - duration.asMillis(), "wall time"));
	}

	/** The time from `earlier` to this one, zero if `earlier` is later. */
	since(earlier: Wall): Duration {
		return this[VALUE] > earlier[VALUE] ? Duration.millis(this[VALUE] - earlier[VALUE]) : Duration.ZERO;
	}

	/** Whether both are the same time. */
	eq(other: Wall): boolean {
		return this[VALUE] === other[VALUE];
	}

	/** -1, 0, or 1 as this is less than, equal to, or greater than `other`, for sorting. */
	cmp(other: Wall): -1 | 0 | 1 {
		return this[VALUE] < other[VALUE] ? -1 : this[VALUE] > other[VALUE] ? 1 : 0;
	}

	/** Whether this time is before `other`. */
	lt(other: Wall): boolean {
		return this[VALUE] < other[VALUE];
	}

	/** Whether this time is at or before `other`. */
	le(other: Wall): boolean {
		return this[VALUE] <= other[VALUE];
	}

	/** Whether this time is after `other`. */
	gt(other: Wall): boolean {
		return this[VALUE] > other[VALUE];
	}

	/** Whether this time is at or after `other`. */
	ge(other: Wall): boolean {
		return this[VALUE] >= other[VALUE];
	}

	/** The time as an ISO 8601 string, or Unix milliseconds past `Date`'s range. */
	toString(): string {
		// biome-ignore lint/style/noRestrictedGlobals: formats a time, never reads the host clock.
		return this[VALUE] <= 8.64e15 ? new Date(this[VALUE]).toISOString() : `Wall(${this[VALUE]}ms)`;
	}

	/** Refuses: a wall time has no unitless number. */
	valueOf(): never {
		return refuse("Wall");
	}

	/** Refuses: serialize `asUnixMillis()` instead. */
	toJSON(): never {
		return refuse("Wall");
	}

	[Symbol.toPrimitive](hint: string): string {
		if (hint === "string") return this.toString();
		return refuse("Wall");
	}
}
