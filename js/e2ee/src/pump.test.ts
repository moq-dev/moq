import { expect, test } from "bun:test";
import { DEFAULT_PUMP_DEPTH, TAG_LEN } from "./constants.ts";
import { Pump } from "./pump.ts";

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

test("completions are released in submit order", async () => {
	const pump = new Pump({ depth: 4, queue: 4 });
	const started: number[] = [];
	const finished: number[] = [];

	const jobs = [0, 1, 2].map((id) =>
		pump
			.submit(async () => {
				started.push(id);
				await sleep(id === 0 ? 30 : 5);
				return id;
			})
			.then((value) => {
				finished.push(value);
				return value;
			}),
	);

	expect(await Promise.all(jobs)).toEqual([0, 1, 2]);
	expect(started).toEqual([0, 1, 2]);
	expect(finished).toEqual([0, 1, 2]);
});

test("depth bounds in-flight work and extra submits wait", async () => {
	const pump = new Pump({ depth: 2, queue: 2 });
	let running = 0;
	let max = 0;
	const gate = Promise.withResolvers<void>();

	const jobs = [0, 1, 2].map(() =>
		pump.submit(async () => {
			running++;
			max = Math.max(max, running);
			await gate.promise;
			running--;
		}),
	);

	await sleep(5);
	expect(max).toBe(2);
	expect(pump.size).toBe(3);
	gate.resolve();
	await Promise.all(jobs);
});

test("a full waiter queue is saturation, not an unbounded promise list", async () => {
	const pump = new Pump({ depth: 1, queue: 1 });
	const gate = Promise.withResolvers<void>();
	const first = pump.submit(() => gate.promise);
	const second = pump.submit(async () => {});
	await sleep(5);
	expect(pump.saturated).toBe(true);
	await expect(pump.submit(async () => {})).rejects.toThrow("e2ee pump saturated");
	gate.resolve();
	await Promise.all([first, second]);
});

test("close rejects waiters and later submits", async () => {
	const pump = new Pump({ depth: 1, queue: 2 });
	const gate = Promise.withResolvers<void>();
	const first = pump
		.submit(() => gate.promise)
		.then(
			() => "resolved",
			(error: unknown) => (error instanceof Error ? error.message : String(error)),
		);
	const second = pump
		.submit(async () => "nope")
		.then(
			() => "resolved",
			(error: unknown) => (error instanceof Error ? error.message : String(error)),
		);
	pump.close(new Error("cancelled"));
	expect(await first).toBe("cancelled");
	expect(await second).toBe("cancelled");
	await expect(pump.submit(async () => {})).rejects.toThrow("cancelled");
	gate.resolve();
});

test("encoder failure fails the pump and later work", async () => {
	const pump = new Pump({ depth: 2, queue: 2 });
	const failed = pump.submit(async () => {
		throw new Error("encoder");
	});
	await expect(failed).rejects.toThrow("encoder");
	await expect(pump.submit(async () => {})).rejects.toThrow("encoder");
});

test("20 ms Opus-sized encrypt stays well under the frame interval", async () => {
	const key = await crypto.subtle.importKey("raw", new Uint8Array(16), "AES-GCM", false, ["encrypt"]);
	const iv = new Uint8Array(12);
	const opus = new Uint8Array(160);
	crypto.getRandomValues(opus);

	const samples = 64;
	const start = performance.now();
	for (let i = 0; i < samples; i++) {
		iv[11] = i;
		await crypto.subtle.encrypt(
			{ name: "AES-GCM", iv, tagLength: 128, additionalData: new Uint8Array() },
			key,
			opus,
		);
	}
	const mean = (performance.now() - start) / samples;
	expect(mean).toBeLessThan(20);
	expect(DEFAULT_PUMP_DEPTH).toBe(8);
	expect(TAG_LEN).toBe(16);
});
