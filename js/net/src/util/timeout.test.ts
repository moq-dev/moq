import { expect, test } from "bun:test";
import { TimeoutError, withTimeout } from "./timeout.ts";

test("withTimeout: resolves before deadline", async () => {
	const result = await withTimeout(Promise.resolve(42), 100, "should not fire");
	expect(result).toBe(42);
});

test("withTimeout: rejects with TimeoutError after deadline", async () => {
	const pending = new Promise<number>(() => {
		// never settles
	});
	await expect(withTimeout(pending, 10, "timed out")).rejects.toBeInstanceOf(TimeoutError);
});

test("withTimeout: passes through promise rejection", async () => {
	const failure = Promise.reject(new Error("boom"));
	await expect(withTimeout(failure, 100, "should not fire")).rejects.toThrow("boom");
});
