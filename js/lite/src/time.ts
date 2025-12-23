export type Nano = number & { readonly _brand: "nano" };

export const Nano = {
	zero: 0 as Nano,
	fromMicro: (us: Micro): Nano => (us * 1_000) as Nano,
	fromMilli: (ms: Milli): Nano => (ms * 1_000_000) as Nano,
	fromSecond: (s: Second): Nano => (s * 1_000_000_000) as Nano,
	toMicro: (ns: Nano): Micro => (ns / 1_000) as Micro,
	toMilli: (ns: Nano): Milli => (ns / 1_000_000) as Milli,
	toSecond: (ns: Nano): Second => (ns / 1_000_000_000) as Second,
} as const;

export type Micro = number & { readonly _brand: "micro" };

export const Micro = {
	zero: 0 as Micro,
	fromNano: (ns: Nano): Micro => (ns / 1_000) as Micro,
	fromMilli: (ms: Milli): Micro => (ms * 1_000) as Micro,
	fromSecond: (s: Second): Micro => (s * 1_000_000) as Micro,
	toNano: (us: Micro): Nano => (us * 1_000) as Nano,
	toMilli: (us: Micro): Milli => (us / 1_000) as Milli,
	toSecond: (us: Micro): Second => (us / 1_000_000) as Second,
} as const;

export type Milli = number & { readonly _brand: "milli" };

export const Milli = {
	zero: 0 as Milli,
	fromNano: (ns: Nano): Milli => (ns / 1_000_000) as Milli,
	fromMicro: (us: Micro): Milli => (us / 1_000) as Milli,
	fromSecond: (s: Second): Milli => (s * 1_000) as Milli,
	toNano: (ms: Milli): Nano => (ms * 1_000_000) as Nano,
	toMicro: (ms: Milli): Micro => (ms * 1_000) as Micro,
	toSecond: (ms: Milli): Second => (ms / 1_000) as Second,
} as const;

export type Second = number & { readonly _brand: "second" };

export const Second = {
	zero: 0 as Second,
	fromNano: (ns: Nano): Second => (ns / 1_000_000_000) as Second,
	fromMicro: (us: Micro): Second => (us / 1_000_000) as Second,
	fromMilli: (ms: Milli): Second => (ms / 1_000) as Second,
	toNano: (s: Second): Nano => (s * 1_000_000_000) as Nano,
	toMicro: (s: Second): Micro => (s * 1_000_000) as Micro,
	toMilli: (s: Second): Milli => (s * 1_000) as Milli,
} as const;
