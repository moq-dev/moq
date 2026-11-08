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

// Package-private bridges for backends that count in plain numbers.
/** @internal */
export let instantOf: (nanos: number) => Instant;
/** @internal */
export let instantNanos: (at: Instant) => number;
/** @internal */
export let wallOf: (unixMillis: number) => Wall;
/** @internal */
export let wallMillis: (at: Wall) => number;

/**
 * A nonnegative span of time, mirroring Rust's `std::time::Duration`.
 *
 * Build one with a unit (`Duration.millis(5)`) and read it with one (`asMillis()`). It never
 * converts to a number implicitly: arithmetic, `<`, and `JSON.stringify` throw.
 */
export class Duration {
	/** No time at all. */
	static readonly ZERO: Duration = new Duration(0);

	/** @internal Nanoseconds. */
	readonly [VALUE]: number;
	declare private readonly kind: "duration";

	private constructor(nanos: number) {
		this[VALUE] = nanos;
	}

	/** A duration of `value` nanoseconds; throws a `RangeError` unless finite and nonnegative. */
	static nanos(value: number): Duration {
		return new Duration(checked(value, "duration"));
	}

	/** A duration of `value` microseconds; throws a `RangeError` unless finite and nonnegative. */
	static micros(value: number): Duration {
		return new Duration(checked(value * 1_000, "duration"));
	}

	/** A duration of `value` milliseconds; throws a `RangeError` unless finite and nonnegative. */
	static millis(value: number): Duration {
		return new Duration(checked(value * 1_000_000, "duration"));
	}

	/** A duration of `value` seconds; throws a `RangeError` unless finite and nonnegative. */
	static secs(value: number): Duration {
		return new Duration(checked(value * 1_000_000_000, "duration"));
	}

	/** The duration in nanoseconds, possibly fractional. */
	asNanos(): number {
		return this[VALUE];
	}

	/** The duration in microseconds, possibly fractional. */
	asMicros(): number {
		return this[VALUE] / 1_000;
	}

	/** The duration in milliseconds, possibly fractional. */
	asMillis(): number {
		return this[VALUE] / 1_000_000;
	}

	/** The duration in seconds, possibly fractional. */
	asSecs(): number {
		return this[VALUE] / 1_000_000_000;
	}

	/** Whether this is no time at all. */
	isZero(): boolean {
		return this[VALUE] === 0;
	}

	/** The sum of both durations. */
	add(other: Duration): Duration {
		return new Duration(checked(this[VALUE] + other[VALUE], "duration"));
	}

	/** This duration minus `other`; throws a `RangeError` if `other` is longer. */
	sub(other: Duration): Duration {
		return new Duration(checked(this[VALUE] - other[VALUE], "duration"));
	}

	/** This duration minus `other`, or zero if `other` is longer. */
	saturatingSub(other: Duration): Duration {
		return this[VALUE] > other[VALUE] ? new Duration(this[VALUE] - other[VALUE]) : Duration.ZERO;
	}

	/** This duration scaled by a finite, nonnegative factor. */
	mul(factor: number): Duration {
		return new Duration(checked(this[VALUE] * factor, "duration"));
	}

	/** This duration divided by a finite, positive divisor. */
	div(divisor: number): Duration {
		if (!(divisor > 0)) throw new RangeError(`invalid divisor: ${divisor}`);
		return new Duration(checked(this[VALUE] / divisor, "duration"));
	}

	/** How many times `other` fits in this duration, possibly fractional; throws a `RangeError` if `other` is zero. */
	divDuration(other: Duration): number {
		if (other[VALUE] === 0) throw new RangeError("division by a zero duration");
		return this[VALUE] / other[VALUE];
	}

	/** The distance between both durations, whichever is longer. */
	absDiff(other: Duration): Duration {
		return new Duration(Math.abs(this[VALUE] - other[VALUE]));
	}

	/** Whether both durations are the same length. */
	eq(other: Duration): boolean {
		return this[VALUE] === other[VALUE];
	}

	/** -1, 0, or 1 as this is less than, equal to, or greater than `other`, for sorting. */
	cmp(other: Duration): -1 | 0 | 1 {
		return this[VALUE] < other[VALUE] ? -1 : this[VALUE] > other[VALUE] ? 1 : 0;
	}

	/** Whether this duration is shorter than `other`. */
	lt(other: Duration): boolean {
		return this[VALUE] < other[VALUE];
	}

	/** Whether this duration is at most `other`. */
	le(other: Duration): boolean {
		return this[VALUE] <= other[VALUE];
	}

	/** Whether this duration is longer than `other`. */
	gt(other: Duration): boolean {
		return this[VALUE] > other[VALUE];
	}

	/** Whether this duration is at least `other`. */
	ge(other: Duration): boolean {
		return this[VALUE] >= other[VALUE];
	}

	/** The shorter of both durations. */
	min(other: Duration): Duration {
		return other[VALUE] < this[VALUE] ? other : this;
	}

	/** The longer of both durations. */
	max(other: Duration): Duration {
		return other[VALUE] > this[VALUE] ? other : this;
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
