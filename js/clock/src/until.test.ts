import { afterEach, expect, test } from "bun:test";
import { Duration, type Installed, Instant, now, sleep, timeoutSignal, Unsettled, until } from "./index.ts";
import { Manual } from "./test.ts";

const installs: Installed[] = [];
afterEach(() => {
	for (const installed of installs.splice(0).reverse()) installed.close();
});

// Drive `run` to completion on a Manual clock, returning its result and when it ended.
async function onManual<T>(run: () => Promise<T>): Promise<{ result: T; at: number }> {
	const clock = new Manual();
	installs.push(clock.install());
	const result = await clock.drive(run());
	return { result, at: ms() };
}
const ms = (): number => now().since(Instant.ZERO).asMillis();

test("until reads once per interval and returns the first ready state", async () => {
	const reads: Array<{ at: number; left: number }> = [];
	const { result, at } = await onManual(() =>
		until({
			read: async (left) => {
				reads.push({ at: ms(), left: left.asMillis() });
				return reads.length;
			},
			ready: (n) => n === 3,
			watchdog: Duration.secs(10),
		}),
	);
	expect(result).toBe(3);
	expect(at).toBe(2_000);
	// Each read gets only what is left of the one deadline.
	expect(reads).toEqual([
		{ at: 0, left: 10_000 },
		{ at: 1_000, left: 9_000 },
		{ at: 2_000, left: 8_000 },
	]);
});

test("until ends at the watchdog, not past it, carrying the last state read", async () => {
	let reads = 0;
	const { result, at } = await onManual(() =>
		until({
			read: async () => ({ renditions: ++reads }),
			ready: () => false,
			watchdog: Duration.millis(2_500),
			every: Duration.secs(1),
		}),
	);
	expect(at).toBe(2_500);
	expect(reads).toBe(3);
	expect(result).toBeInstanceOf(Unsettled);
	const unsettled = result as Unsettled<{ renditions: number }>;
	expect(unsettled.last).toEqual({ renditions: 3 });
	expect(unsettled.message).toBe('never settled within 2500ms; last read: {"renditions":3}');
});

test("until bounds a read that ignores its budget", async () => {
	let reads = 0;
	const { result, at } = await onManual(() =>
		until({
			read: async () => {
				if (++reads === 2) await sleep(Duration.secs(60));
				return reads;
			},
			ready: () => false,
			watchdog: Duration.secs(5),
		}),
	);
	expect(at).toBe(5_000);
	expect((result as Unsettled<number>).last).toBe(1);
});

test("until holds its deadline through the read's synchronous work", async () => {
	const clock = new Manual();
	installs.push(clock.install());
	let result: unknown;
	void until({
		read: () => {
			clock.jump(Duration.secs(3));
			return new Promise<never>(() => {});
		},
		ready: () => true,
		watchdog: Duration.secs(5),
	}).then((r) => {
		result = r;
	});
	await clock.advance(Duration.secs(2));
	expect(ms()).toBe(5_000);
	expect(result).toBeInstanceOf(Unsettled);
});

test("until refuses a zero period", async () => {
	const wait = until({ read: async () => 1, ready: () => false, watchdog: Duration.secs(1), every: Duration.ZERO });
	await expect(wait).rejects.toThrow(RangeError);
});

test("until treats a read aborted by its own budget as the watchdog", async () => {
	let reads = 0;
	const { result, at } = await onManual(() =>
		until({
			read: async (left) => {
				if (++reads === 1) return "starting";
				await sleep(Duration.secs(60), { signal: timeoutSignal(left) });
				return "never";
			},
			ready: () => false,
			watchdog: Duration.secs(5),
		}),
	);
	expect(at).toBe(5_000);
	expect((result as Unsettled<string>).last).toBe("starting");
});

test("until rethrows a read failure before the deadline at once, whatever its type", async () => {
	for (const failure of [new Error("401"), new DOMException("signal timed out", "TimeoutError")]) {
		let reads = 0;
		const run = onManual(() =>
			until({
				read: async () => {
					reads++;
					throw failure;
				},
				ready: () => true,
				watchdog: Duration.secs(5),
			}),
		);
		await expect(run).rejects.toBe(failure);
		expect(reads).toBe(1);
		expect(ms()).toBe(0);
		installs.pop()?.close();
	}
});

test("Unsettled shortens a long state in its message", () => {
	const message = new Unsettled(Duration.secs(1), "x".repeat(2_000)).message;
	expect(message).toBe(`never settled within 1000ms; last read: ${"x".repeat(500)}...`);
});
