export type Nano = number & { readonly _brand: "nano" };

export const Nano = {
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
	now: (): Micro => (performance.now() * 1_000) as Micro,
	add: (a: Micro, b: Micro): Micro => (a + b) as Micro,
	sub: (a: Micro, b: Micro): Micro => (a - b) as Micro,
	mul: (a: Micro, b: number): Micro => (a * b) as Micro,
	div: (a: Micro, b: number): Micro => (a / b) as Micro,
	max: (a: Micro, b: Micro): Micro => Math.max(a, b) as Micro,
	min: (a: Micro, b: Micro): Micro => Math.min(a, b) as Micro,
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
	now: (): Milli => performance.now() as Milli,
	add: (a: Milli, b: Milli): Milli => (a + b) as Milli,
	sub: (a: Milli, b: Milli): Milli => (a - b) as Milli,
	mul: (a: Milli, b: number): Milli => (a * b) as Milli,
	div: (a: Milli, b: number): Milli => (a / b) as Milli,
	max: (a: Milli, b: Milli): Milli => Math.max(a, b) as Milli,
	min: (a: Milli, b: Milli): Milli => Math.min(a, b) as Milli,
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
	now: (): Second => (performance.now() / 1_000) as Second,
	add: (a: Second, b: Second): Second => (a + b) as Second,
	sub: (a: Second, b: Second): Second => (a - b) as Second,
	mul: (a: Second, b: number): Second => (a * b) as Second,
	div: (a: Second, b: number): Second => (a / b) as Second,
	max: (a: Second, b: Second): Second => Math.max(a, b) as Second,
	min: (a: Second, b: Second): Second => Math.min(a, b) as Second,
} as const;
