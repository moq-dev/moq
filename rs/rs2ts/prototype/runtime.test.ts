import { expect, test } from "bun:test";
import { U64 } from "../../../js/net/src/util/u64";
import { unzigzag as generatedUnzigzag, zigzag as generatedZigzag } from "./generated";
import {
	add,
	and,
	copies,
	drops,
	Event,
	neg,
	type Option,
	Owner,
	sar,
	shl,
	shr,
	sub,
	unwind,
	unzigzag,
	xor,
	zigzag,
} from "./runtime";

test("integer and lifetime mappings agree with native Rust", () => {
	const path = process.env.RS2TS_PROBE;
	if (!path) throw new Error("run through the prototype justfile so the Rust oracle is compiled");
	const result = Bun.spawnSync([path]);
	expect(result.exitCode).toBe(0);
	const rows = result.stdout.toString().trim().split("\n");
	let integers = 0;
	for (const row of rows) {
		const [op, a, b, answer] = row.split(" ");
		if (["nested", "drops", "unwind", "copies"].includes(op)) continue;
		const value = U64.fromBigInt(BigInt.asUintN(64, BigInt(a)));
		if (op === "neg") {
			expect(BigInt.asIntN(64, neg(value).toBigInt())).toBe(BigInt(b));
		} else if (op === "sar") {
			expect(BigInt.asIntN(64, sar(value, Number(b)).toBigInt())).toBe(BigInt(answer));
		} else if (op === "and" || op === "xor") {
			expect((op === "and" ? and : xor)(value, U64.fromBigInt(BigInt(b))).toBigInt()).toBe(BigInt(answer));
		} else if (op === "zigzag") {
			expect(zigzag(value).toBigInt()).toBe(BigInt(b));
			expect(generatedZigzag(value).toBigInt()).toBe(BigInt(b));
		} else if (op === "unzigzag") {
			expect(BigInt.asIntN(64, unzigzag(value).toBigInt())).toBe(BigInt(b));
			expect(BigInt.asIntN(64, generatedUnzigzag(value).toBigInt())).toBe(BigInt(b));
		} else if (op === "shl" || op === "shr") {
			expect((op === "shl" ? shl : shr)(value, Number(b)).toBigInt()).toBe(BigInt(answer));
		} else if (op === "add" || op === "sub") {
			const run = () => (op === "add" ? add : sub)(value, U64.fromBigInt(BigInt(b))).toBigInt();
			if (answer === "overflow") expect(run).toThrow(RangeError);
			else expect(run()).toBe(BigInt(answer));
		} else throw new Error(`unknown oracle row: ${row}`);
		integers++;
	}
	expect(integers).toBe(265 * 60);
	expect(rows.at(-4)).toBe("nested 0 1 2");
	expect(rows.at(-3)).toBe(`drops [${drops(false).join(", ")}] [${drops(true).join(", ")}]`);
	expect(rows.at(-2)).toBe(`unwind [${unwind().join(", ")}]`);
	expect(rows.at(-1)).toBe(`copies (${copies().join(", ")})`);
});

test("nested optional states survive tagged representation", () => {
	const values: Option<Option<U64>>[] = [
		{ tag: "none" },
		{ tag: "some", value: { tag: "none" } },
		{ tag: "some", value: { tag: "some", value: U64.MAX } },
	];
	expect(values.map((value) => (value.tag === "none" ? 0 : value.value.tag === "none" ? 1 : 2))).toEqual([0, 1, 2]);
	// Erasing both optional layers cannot represent the first two states.
	const erased: (U64 | undefined)[] = [undefined, undefined, U64.MAX];
	expect(new Set(erased).size).toBe(2);
});

test("moves invalidate handles and clones keep the payload alive", () => {
	const events: number[] = [];
	const first = new Owner(new Event(1, events));
	const clone = first.clone();
	const moved = first.move();
	expect(() => first.clone()).toThrow("used after move or drop");
	expect(() => first[Symbol.dispose]()).toThrow("used after move or drop");
	moved[Symbol.dispose]();
	expect(events).toEqual([]);
	clone[Symbol.dispose]();
	expect(events).toEqual([1]);
	expect(() => clone[Symbol.dispose]()).toThrow("used after move or drop");
});

test("the 32-bit shift boundary does not use JavaScript's modulo-32 shift", () => {
	expect(shl(U64.fromNumber(1), 32)).toEqual(new U64(1, 0));
	expect(shr(new U64(1, 0), 32)).toEqual(U64.fromNumber(1));
	for (const bits of [-1, 64, 1.5, Number.NaN]) {
		expect(() => shl(U64.ZERO, bits)).toThrow(RangeError);
		expect(() => shr(U64.ZERO, bits)).toThrow(RangeError);
	}
});
