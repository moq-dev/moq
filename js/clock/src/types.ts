function checked(value: number, what: string): number {
	if (!Number.isFinite(value) || value < 0) throw new RangeError(`invalid ${what}: ${value}`);
	return value;
}

function refuse(what: string): never {
	throw new TypeError(`a ${what} is not a number; read it with an explicit unit`);
}

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

	readonly #nanos: number;

	private constructor(nanos: number) {
		this.#nanos = nanos;
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
		return this.#nanos;
	}

	/** The duration in microseconds, possibly fractional. */
	asMicros(): number {
		return this.#nanos / 1_000;
	}

	/** The duration in milliseconds, possibly fractional. */
	asMillis(): number {
		return this.#nanos / 1_000_000;
	}

	/** The duration in seconds, possibly fractional. */
	asSecs(): number {
		return this.#nanos / 1_000_000_000;
	}

	/** Whether this is no time at all. */
	isZero(): boolean {
		return this.#nanos === 0;
	}

	/** The sum of both durations. */
	add(other: Duration): Duration {
		return new Duration(checked(this.#nanos + other.#nanos, "duration"));
	}

	/** This duration minus `other`; throws a `RangeError` if `other` is longer. */
	sub(other: Duration): Duration {
		return new Duration(checked(this.#nanos - other.#nanos, "duration"));
	}

	/** This duration minus `other`, or zero if `other` is longer. */
	saturatingSub(other: Duration): Duration {
		return this.#nanos > other.#nanos ? new Duration(this.#nanos - other.#nanos) : Duration.ZERO;
	}

	/** This duration scaled by a finite, nonnegative factor. */
	mul(factor: number): Duration {
		return new Duration(checked(this.#nanos * factor, "duration"));
	}

	/** This duration divided by a finite, positive divisor. */
	div(divisor: number): Duration {
		if (!(divisor > 0)) throw new RangeError(`invalid divisor: ${divisor}`);
		return new Duration(checked(this.#nanos / divisor, "duration"));
	}

	/** How many times `other` fits in this duration, possibly fractional; throws a `RangeError` if `other` is zero. */
	divDuration(other: Duration): number {
		if (other.#nanos === 0) throw new RangeError("division by a zero duration");
		return this.#nanos / other.#nanos;
	}

	/** The distance between both durations, whichever is longer. */
	absDiff(other: Duration): Duration {
		return new Duration(Math.abs(this.#nanos - other.#nanos));
	}

	/** Whether both durations are the same length. */
	eq(other: Duration): boolean {
		return this.#nanos === other.#nanos;
	}

	/** Whether this duration is shorter than `other`. */
	lt(other: Duration): boolean {
		return this.#nanos < other.#nanos;
	}

	/** Whether this duration is at most `other`. */
	le(other: Duration): boolean {
		return this.#nanos <= other.#nanos;
	}

	/** Whether this duration is longer than `other`. */
	gt(other: Duration): boolean {
		return this.#nanos > other.#nanos;
	}

	/** Whether this duration is at least `other`. */
	ge(other: Duration): boolean {
		return this.#nanos >= other.#nanos;
	}

	/** The shorter of both durations. */
	min(other: Duration): Duration {
		return other.#nanos < this.#nanos ? other : this;
	}

	/** The longer of both durations. */
	max(other: Duration): Duration {
		return other.#nanos > this.#nanos ? other : this;
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

	readonly #nanos: number;

	private constructor(nanos: number) {
		this.#nanos = nanos;
	}

	static {
		instantOf = (nanos) => new Instant(checked(nanos, "instant"));
		instantNanos = (at) => at.#nanos;
	}

	/** This instant moved forward by `duration`. */
	add(duration: Duration): Instant {
		return new Instant(checked(this.#nanos + duration.asNanos(), "instant"));
	}

	/** This instant moved back by `duration`; throws a `RangeError` before the clock's origin. */
	sub(duration: Duration): Instant {
		return new Instant(checked(this.#nanos - duration.asNanos(), "instant"));
	}

	/** The time from `earlier` to this instant, zero if `earlier` is later. */
	since(earlier: Instant): Duration {
		return this.#nanos > earlier.#nanos ? Duration.nanos(this.#nanos - earlier.#nanos) : Duration.ZERO;
	}

	/** Whether both are the same instant. */
	eq(other: Instant): boolean {
		return this.#nanos === other.#nanos;
	}

	/** Whether this instant is before `other`. */
	lt(other: Instant): boolean {
		return this.#nanos < other.#nanos;
	}

	/** Whether this instant is at or before `other`. */
	le(other: Instant): boolean {
		return this.#nanos <= other.#nanos;
	}

	/** Whether this instant is after `other`. */
	gt(other: Instant): boolean {
		return this.#nanos > other.#nanos;
	}

	/** Whether this instant is at or after `other`. */
	ge(other: Instant): boolean {
		return this.#nanos >= other.#nanos;
	}

	/** The earlier of both instants. */
	min(other: Instant): Instant {
		return other.#nanos < this.#nanos ? other : this;
	}

	/** The later of both instants. */
	max(other: Instant): Instant {
		return other.#nanos > this.#nanos ? other : this;
	}

	/** The time since the origin with a unit, such as `Instant(1.5ms)`. */
	toString(): string {
		return `Instant(${this.#nanos / 1_000_000}ms)`;
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
 * Convert Unix milliseconds with `Wall.UNIX_EPOCH.add(Duration.millis(ms))` and back with
 * `wall.since(Wall.UNIX_EPOCH).asMillis()`.
 */
export class Wall {
	/** 1970-01-01T00:00:00Z. */
	static readonly UNIX_EPOCH: Wall = new Wall(0);

	// Milliseconds, since Unix nanoseconds pass 2^53.
	readonly #millis: number;

	private constructor(millis: number) {
		this.#millis = millis;
	}

	static {
		wallOf = (millis) => new Wall(checked(millis, "wall time"));
		wallMillis = (at) => at.#millis;
	}

	/** This time moved forward by `duration`. */
	add(duration: Duration): Wall {
		return new Wall(checked(this.#millis + duration.asMillis(), "wall time"));
	}

	/** This time moved back by `duration`; throws a `RangeError` before the Unix epoch. */
	sub(duration: Duration): Wall {
		return new Wall(checked(this.#millis - duration.asMillis(), "wall time"));
	}

	/** The time from `earlier` to this one, zero if `earlier` is later. */
	since(earlier: Wall): Duration {
		return this.#millis > earlier.#millis ? Duration.millis(this.#millis - earlier.#millis) : Duration.ZERO;
	}

	/** Whether both are the same time. */
	eq(other: Wall): boolean {
		return this.#millis === other.#millis;
	}

	/** Whether this time is before `other`. */
	lt(other: Wall): boolean {
		return this.#millis < other.#millis;
	}

	/** Whether this time is at or before `other`. */
	le(other: Wall): boolean {
		return this.#millis <= other.#millis;
	}

	/** Whether this time is after `other`. */
	gt(other: Wall): boolean {
		return this.#millis > other.#millis;
	}

	/** Whether this time is at or after `other`. */
	ge(other: Wall): boolean {
		return this.#millis >= other.#millis;
	}

	/** The time as an ISO 8601 string, or Unix milliseconds past `Date`'s range. */
	toString(): string {
		return this.#millis <= 8.64e15 ? new Date(this.#millis).toISOString() : `Wall(${this.#millis}ms)`;
	}

	/** Refuses: a wall time has no unitless number. */
	valueOf(): never {
		return refuse("Wall");
	}

	/** Refuses: serialize `since(Wall.UNIX_EPOCH)` with an explicit unit instead. */
	toJSON(): never {
		return refuse("Wall");
	}

	[Symbol.toPrimitive](hint: string): string {
		if (hint === "string") return this.toString();
		return refuse("Wall");
	}
}
