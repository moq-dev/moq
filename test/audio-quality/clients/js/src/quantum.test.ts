import { describe, expect, test } from "bun:test";
import { classify, Ledger } from "./quantum.ts";

const quantum = (values: number[], channels = 2) => Array.from({ length: channels }, () => Float32Array.from(values));
const tone = (length: number) => Array.from({ length }, (_, i) => 0.5 * Math.sin(i / 3 + 0.1));

describe("classify", () => {
	test("a quantum the ring filled is full", () => {
		expect(classify(quantum(tone(128))).fill).toBe("full");
	});

	test("trailing zeros in every channel are the samples the ring did not have", () => {
		const values = [...tone(100), ...new Array(28).fill(0)];
		expect(classify(quantum(values))).toMatchObject({ fill: "short", missing: 28 });
	});

	test("one channel still playing means the ring supplied that sample", () => {
		const left = Float32Array.from([...tone(100), ...new Array(28).fill(0)]);
		const right = Float32Array.from(tone(128));
		expect(classify([left, right])).toMatchObject({ fill: "full", missing: 0 });
	});

	test("all zeros is silent, however it came about", () => {
		expect(classify(quantum(new Array(128).fill(0)))).toMatchObject({ fill: "silent", missing: 128 });
		expect(classify([])).toMatchObject({ fill: "silent", missing: 0 });
	});
});

describe("Ledger", () => {
	const RATE = 48000;
	const full = classify(quantum(tone(128)));
	const short = classify(quantum([...tone(64), ...new Array(64).fill(0)]));
	const silent = classify(quantum(new Array(128).fill(0)));

	test("the tune-in before the first audible quantum is not a gap", () => {
		const ledger = new Ledger(RATE, 0.001);
		ledger.add(0, 128, silent);
		ledger.add(128, 128, silent);
		ledger.add(256, 128, full);
		expect(ledger.counts).toEqual({ quanta: 1, quiet: 0 });
		expect(ledger.take()).toEqual([]);
	});

	test("consecutive unfilled quanta are one gap, closed by the next full one", () => {
		const ledger = new Ledger(RATE, 0.001);
		ledger.add(0, 128, full);
		ledger.add(128, 128, short);
		ledger.add(256, 128, silent);
		ledger.add(384, 128, short);
		expect(ledger.take()).toEqual([]);

		ledger.add(512, 128, full);
		const [gap, ...rest] = ledger.take();
		expect(rest).toEqual([]);
		// Starts at the first missing sample, half-way through the first short quantum.
		expect(gap?.at).toBeCloseTo((192 / RATE) * 1000);
		expect(gap?.ms).toBeCloseTo(((64 + 128 + 64) / RATE) * 1000);
		expect(gap).toMatchObject({ quanta: 3, short: 2 });
		expect(ledger.counts).toEqual({ quanta: 5, quiet: 1 });
	});
});
