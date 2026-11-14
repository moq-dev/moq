import { expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { TimeoutError, withTimeout } from "./timeout.ts";

test("withTimeout: resolves before deadline", async () => {
	const result = await withTimeout(Promise.resolve(42), Duration.nanos(100), "should not fire");
	expect(result).toBe(42);
});

test("withTimeout: rejects with TimeoutError after deadline", async () => {
	const pending = new Promise<number>(() => {
		// never settles
	});
	const clock = new Manual();
	const installed = clock.install();
	try {
		const timed = withTimeout(pending, Duration.nanos(10), "timed out").catch((err: unknown) => err);
		await clock.advance(Duration.nanos(9));
		expect(await Promise.race([timed, "pending"])).toBe("pending");
		await clock.advance(Duration.nanos(1));
		expect(await timed).toBeInstanceOf(TimeoutError);
	} finally {
		installed.close();
	}
});

test("withTimeout: passes through promise rejection", async () => {
	const failure = Promise.reject(new Error("boom"));
	await expect(withTimeout(failure, Duration.nanos(100), "should not fire")).rejects.toThrow("boom");
});
