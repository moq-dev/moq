function checked(value: number): number {
	if (!Number.isFinite(value) || value < 0) throw new RangeError(`invalid time: ${value}`);
	return value;
}

/** A nonnegative duration in nanoseconds. */
export type Nano = number & { readonly _brand: "clock-nano" };

/** Brand nanoseconds or convert another duration unit. */
export const Nano = Object.assign((value: number): Nano => checked(value) as Nano, {
	zero: 0 as Nano,
	fromMicro: (value: Micro): Nano => Nano(value * 1_000),
	toMicro: (value: Nano): Micro => Micro(value / 1_000),
	fromMilli: (value: Milli): Nano => Nano(value * 1_000_000),
	toMilli: (value: Nano): Milli => Milli(value / 1_000_000),
	fromSecond: (value: Second): Nano => Nano(value * 1_000_000_000),
	toSecond: (value: Nano): Second => Second(value / 1_000_000_000),
});

/** A nonnegative duration in microseconds. */
export type Micro = number & { readonly _brand: "clock-micro" };

/** Brand microseconds or convert another duration unit. */
export const Micro = Object.assign((value: number): Micro => checked(value) as Micro, {
	zero: 0 as Micro,
	fromNano: (value: Nano): Micro => Micro(value / 1_000),
	toNano: (value: Micro): Nano => Nano(value * 1_000),
	fromMilli: (value: Milli): Micro => Micro(value * 1_000),
	toMilli: (value: Micro): Milli => Milli(value / 1_000),
	fromSecond: (value: Second): Micro => Micro(value * 1_000_000),
	toSecond: (value: Micro): Second => Second(value / 1_000_000),
});

/** A nonnegative duration in milliseconds. */
export type Milli = number & { readonly _brand: "clock-milli" };

/** Brand milliseconds or convert another duration unit. */
export const Milli = Object.assign((value: number): Milli => checked(value) as Milli, {
	zero: 0 as Milli,
	fromNano: (value: Nano): Milli => Milli(value / 1_000_000),
	toNano: (value: Milli): Nano => Nano(value * 1_000_000),
	fromMicro: (value: Micro): Milli => Milli(value / 1_000),
	toMicro: (value: Milli): Micro => Micro(value * 1_000),
	fromSecond: (value: Second): Milli => Milli(value * 1_000),
	toSecond: (value: Milli): Second => Second(value / 1_000),
});

/** A nonnegative duration in seconds. */
export type Second = number & { readonly _brand: "clock-second" };

/** Brand seconds or convert another duration unit. */
export const Second = Object.assign((value: number): Second => checked(value) as Second, {
	zero: 0 as Second,
	fromNano: (value: Nano): Second => Second(value / 1_000_000_000),
	toNano: (value: Second): Nano => Nano(value * 1_000_000_000),
	fromMicro: (value: Micro): Second => Second(value / 1_000_000),
	toMicro: (value: Second): Micro => Micro(value * 1_000_000),
	fromMilli: (value: Milli): Second => Second(value / 1_000),
	toMilli: (value: Second): Milli => Milli(value * 1_000),
});

/** Nanoseconds since this clock's origin, separate from durations and wall time. */
export type Instant = number & { readonly _brand: "clock-instant" };

/** Brand a logical instant or do arithmetic on its own clock's coordinates. */
export const Instant = Object.assign((value: number): Instant => checked(value) as Instant, {
	zero: 0 as Instant,
	add: (at: Instant, duration: Nano): Instant => Instant(at + Nano(duration)),
	sub: (at: Instant, duration: Nano): Instant => Instant(at - Nano(duration)),
	durationSince: (at: Instant, earlier: Instant): Nano => Nano(Math.max(0, at - earlier)),
});

/** Milliseconds since the Unix epoch, separate from monotonic time. */
export type Wall = number & { readonly _brand: "clock-wall" };

/** Brand Unix milliseconds or move wall time by a duration. */
export const Wall = Object.assign((unixMillis: number): Wall => checked(unixMillis) as Wall, {
	UNIX_EPOCH: 0 as Wall,
	add: (at: Wall, duration: Nano): Wall => Wall(at + Nano.toMilli(duration)),
	sub: (at: Wall, duration: Nano): Wall => Wall(at - Nano.toMilli(duration)),
	durationSince: (at: Wall, earlier: Wall): Nano => Nano.fromMilli(Milli(Math.max(0, at - earlier))),
});
