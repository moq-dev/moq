import { expect, test } from "bun:test";
import { Timescale, Timestamp } from "./time.ts";

test("Timestamp rejects negative values", () => {
	expect(() => new Timestamp(-1, Timescale.MILLI)).toThrow();
	expect(() => Timestamp.fromMicros(-1)).toThrow();
});

test("Timestamp rejects non-finite values", () => {
	expect(() => new Timestamp(Number.NaN, Timescale.MILLI)).toThrow();
	expect(() => Timestamp.fromMillis(Number.POSITIVE_INFINITY)).toThrow();
});
