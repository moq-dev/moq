import { U64 } from "../../../js/net/src/util/u64";

const POW32 = 0x1_0000_0000;

/** Add two u64 values, refusing overflow. */
export function add(left: U64, right: U64): U64 {
	const lo = left.lo + right.lo;
	return new U64(left.hi + right.hi + (lo >= POW32 ? 1 : 0), lo >>> 0);
}

/** Subtract two u64 values, refusing underflow. */
export function sub(left: U64, right: U64): U64 {
	const lo = left.lo - right.lo;
	return new U64(left.hi - right.hi - (lo < 0 ? 1 : 0), lo >>> 0);
}

function shiftAmount(bits: number): void {
	if (!Number.isInteger(bits) || bits < 0 || bits >= 64) throw new RangeError("invalid u64 shift");
}

/** Shift left, discarding bits beyond the u64 width like Rust. */
export function shl(value: U64, bits: number): U64 {
	shiftAmount(bits);
	if (bits === 0) return value;
	if (bits >= 32) return new U64((value.lo << (bits - 32)) >>> 0, 0);
	return new U64(((value.hi << bits) | (value.lo >>> (32 - bits))) >>> 0, (value.lo << bits) >>> 0);
}

/** Shift right, filling the high bits with zero. */
export function shr(value: U64, bits: number): U64 {
	shiftAmount(bits);
	if (bits === 0) return value;
	if (bits >= 32) return new U64(0, value.hi >>> (bits - 32));
	return new U64(value.hi >>> bits, ((value.lo >>> bits) | (value.hi << (32 - bits))) >>> 0);
}

/** Shift an i64 bit pattern right, extending its sign bit. */
export function sar(value: U64, bits: number): U64 {
	shiftAmount(bits);
	if (bits === 0) return value;
	if (bits >= 32) return new U64(value.hi >>> 31 ? 0xffffffff : 0, (value.hi >> (bits - 32)) >>> 0);
	return new U64((value.hi >> bits) >>> 0, ((value.lo >>> bits) | (value.hi << (32 - bits))) >>> 0);
}

/** XOR two 64-bit bit patterns without a lossy number conversion. */
export function xor(left: U64, right: U64): U64 {
	return new U64((left.hi ^ right.hi) >>> 0, (left.lo ^ right.lo) >>> 0);
}

/** AND two 64-bit bit patterns without a lossy number conversion. */
export function and(left: U64, right: U64): U64 {
	return new U64((left.hi & right.hi) >>> 0, (left.lo & right.lo) >>> 0);
}

/** Negate an i64 bit pattern modulo 64 bits, as MIR Neg(Wrap) requires. */
export function neg(value: U64): U64 {
	const lo = -value.lo >>> 0;
	return new U64((~value.hi + (lo === 0 ? 1 : 0)) >>> 0, lo);
}

/** Zigzag encode an i64 stored as its two's complement bit pattern. */
export function zigzag(value: U64): U64 {
	const shifted = shl(value, 1);
	const mask = value.hi >>> 31 ? 0xffffffff : 0;
	return new U64((shifted.hi ^ mask) >>> 0, (shifted.lo ^ mask) >>> 0);
}

/** Zigzag decode into an i64 two's complement bit pattern. */
export function unzigzag(value: U64): U64 {
	const shifted = shr(value, 1);
	const mask = value.lo & 1 ? 0xffffffff : 0;
	return new U64((shifted.hi ^ mask) >>> 0, (shifted.lo ^ mask) >>> 0);
}

/** A tagged option retains None and Some(None) as different values. */
export type Option<T> = { tag: "none" } | { tag: "some"; value: T };

/** A probe payload records its deterministic destruction. */
export class Event {
	readonly id: number;
	readonly events: number[];

	constructor(id: number, events: number[]) {
		this.id = id;
		this.events = events;
	}

	drop(): void {
		this.events.push(this.id);
	}
}

/** An explicit shared owner, corresponding to an Rc handle with drop glue. */
export class Owner {
	#live = true;
	#state: { count: number; value: Event };

	constructor(value: Event) {
		this.#state = { count: 1, value };
	}

	#check(): void {
		if (!this.#live) throw new Error("owner used after move or drop");
	}

	clone(): Owner {
		this.#check();
		const owner = new Owner(this.#state.value);
		owner.#state = this.#state;
		this.#state.count++;
		return owner;
	}

	move(): Owner {
		this.#check();
		const owner = new Owner(this.#state.value);
		owner.#state = this.#state;
		this.#live = false;
		return owner;
	}

	[Symbol.dispose](): void {
		this.#check();
		this.#live = false;
		if (--this.#state.count === 0) this.#state.value.drop();
	}
}

/** Model the probe's normal and early-return MIR drop points. */
export function drops(early: boolean): number[] {
	const events: number[] = [];
	const first = new Owner(new Event(1, events));
	const second = new Owner(new Event(2, events));
	const cloned = first.clone();
	first[Symbol.dispose]();
	const moved = cloned.move();
	second[Symbol.dispose]();
	if (early) {
		moved[Symbol.dispose]();
		return events;
	}
	const third = new Event(3, events);
	const fourth = new Event(4, events);
	fourth.drop();
	third.drop();
	moved[Symbol.dispose]();
	return events;
}

/** Translate lexical unwind edges into finally blocks. */
export function unwind(): number[] {
	const events: number[] = [];
	try {
		const first = new Event(1, events);
		try {
			const second = new Event(2, events);
			try {
				throw new Error("probe");
			} finally {
				second.drop();
			}
		} finally {
			first.drop();
		}
	} catch (error) {
		if (!(error instanceof Error) || error.message !== "probe") throw error;
	}
	return events;
}

/** Copy a struct's fields before a mutable use. */
export function copies(): [number, number] {
	const original = { count: 7 };
	const copy = { ...original };
	copy.count += 1;
	return [original.count, copy.count];
}
