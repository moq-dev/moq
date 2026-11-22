import { expect, test } from "bun:test";
import { Duration, Instant, now } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { DELAYS, settle } from "./propagation.ts";

const secs = (n: number) => Duration.secs(n);

// Serves each spec from its `after`th check on, as a CDN catching up with a publish.
function registry(after: Record<string, number>) {
	const checks: string[] = [];
	const found = (spec: string) => {
		checks.push(spec);
		return checks.filter((s) => s === spec).length >= after[spec];
	};
	return { checks, found };
}

// Runs `body` with a fresh `Manual` clock installed, restoring the ambient clock after.
async function withClock(body: (clock: Manual) => Promise<void>) {
	const clock = new Manual();
	const installed = clock.install();
	try {
		await body(clock);
	} finally {
		installed.close();
	}
}

test("waits out a transient miss and returns nothing missing", () =>
	withClock(async (clock) => {
		const { checks, found } = registry({ "@moq/net@^1": 2 });
		expect(await clock.drive(settle(["@moq/net@^1"], found))).toEqual([]);
		expect(checks).toEqual(["@moq/net@^1", "@moq/net@^1"]);
		expect(now().since(Instant.ZERO).asSecs()).toBe(15);
	}));

test("rechecks only what is still missing", () =>
	withClock(async (clock) => {
		const { checks, found } = registry({ a: 1, b: 3 });
		expect(await clock.drive(settle(["a", "b"], found))).toEqual([]);
		expect(checks).toEqual(["a", "b", "b", "b"]);
	}));

test("returns the specs still missing once the delays run out, after 75 s", () =>
	withClock(async (clock) => {
		const { checks, found } = registry({ never: Number.POSITIVE_INFINITY, late: 2 });
		expect(await clock.drive(settle(["never", "late"], found))).toEqual(["never"]);
		expect(checks.filter((s) => s === "never")).toHaveLength(DELAYS.length);
		expect(now().since(Instant.ZERO).asSecs()).toBe(75);
	}));

test("a spec found at once never waits", () =>
	withClock(async (clock) => {
		expect(await clock.drive(settle([], () => false))).toEqual([]);
		expect(now().since(Instant.ZERO).asSecs()).toBe(0);
	}));

test("a registry failure while rechecking is not swallowed", () =>
	withClock(async (clock) => {
		const failing = () => {
			throw new Error("ECONNRESET");
		};
		const work = settle(["x"], failing, [secs(1)]).catch((e: unknown) => e);
		expect(await clock.drive(work)).toEqual(new Error("ECONNRESET"));
	}));
