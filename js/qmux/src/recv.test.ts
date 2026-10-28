import { describe, expect, test } from "bun:test";
import { replenishWindow } from "./credit.ts";
import { RecvStream } from "./recv.ts";

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("RecvStream", () => {
	test("delivers buffered chunks in order, crediting each on delivery", async () => {
		const consumed: number[] = [];
		const recv = new RecvStream(
			(b) => consumed.push(b),
			() => {},
		);
		recv.push(new Uint8Array([1]));
		recv.push(new Uint8Array([2, 2]));
		recv.push(new Uint8Array([3, 3, 3]));
		recv.finish();

		const reader = recv.readable.getReader();
		expect((await reader.read()).value).toEqual(new Uint8Array([1]));
		expect((await reader.read()).value).toEqual(new Uint8Array([2, 2]));
		expect((await reader.read()).value).toEqual(new Uint8Array([3, 3, 3]));
		expect((await reader.read()).done).toBe(true);

		// One onConsume per delivered chunk, by byte length.
		expect(consumed).toEqual([1, 2, 3]);
	});

	test("does not credit undelivered data (slow-reader backpressure)", async () => {
		let consumed = 0;
		const recv = new RecvStream(
			(b) => {
				consumed += b;
			},
			() => {},
		);
		// Buffer 1000 bytes the application never reads.
		for (let i = 0; i < 10; i++) recv.push(new Uint8Array(100));
		await tick();

		// A zero high-water mark prevents even one chunk from being prefetched into
		// the ReadableStream before the application issues a read.
		expect(consumed).toBe(0);
	});

	test("reading resumes crediting", async () => {
		let consumed = 0;
		const recv = new RecvStream(
			(b) => {
				consumed += b;
			},
			() => {},
		);
		for (let i = 0; i < 5; i++) recv.push(new Uint8Array(10));
		await tick();
		const before = consumed;

		const reader = recv.readable.getReader();
		await reader.read();
		await reader.read();
		await tick();

		expect(consumed).toBeGreaterThan(before);
	});

	test("finish with no data closes immediately", async () => {
		const recv = new RecvStream(
			() => {},
			() => {},
		);
		recv.finish();
		expect((await recv.readable.getReader().read()).done).toBe(true);
	});

	test("error aborts the readable and discards buffered data", async () => {
		const recv = new RecvStream(
			() => {},
			() => {},
		);
		recv.push(new Uint8Array(50));
		expect(recv.error(new Error("RESET_STREAM"))).toBe(50);
		await expect(recv.readable.getReader().read()).rejects.toThrow("RESET_STREAM");
	});

	test("cancel invokes onCancel (STOP_SENDING)", async () => {
		let discarded = -1;
		const recv = new RecvStream(
			() => {},
			(bytes) => {
				discarded = bytes;
			},
		);
		recv.push(new Uint8Array(25));
		await recv.readable.getReader().cancel();
		expect(discarded).toBe(25);
	});
});

describe("replenishWindow", () => {
	const W = 1000n;

	test("no update while more than half the window remains", () => {
		// Fresh window: max=W, consumed=0 → W remaining → no update.
		expect(replenishWindow(0n, W, W)).toBeNull();
		// 400 consumed → 600 remaining (> 500) → no update.
		expect(replenishWindow(400n, W, W)).toBeNull();
	});

	test("replenishes relative to consumed once half is reached", () => {
		// 500 consumed → 500 remaining (≤ half) → new max = consumed + window.
		expect(replenishWindow(500n, W, W)).toBe(1500n);
		expect(replenishWindow(900n, W, W)).toBe(1900n);
	});

	test("bounds the peer to one window beyond consumed", () => {
		// Simulate a greedy peer + reader consuming W/2 per round; the advertised
		// limit must stay exactly consumed + window, never drifting upward.
		let max = W;
		let consumed = 0n;
		for (let round = 0; round < 5; round++) {
			consumed += W / 2n;
			const next = replenishWindow(consumed, max, W);
			expect(next).toBe(consumed + W);
			if (next !== null) max = next;
			// Unacked-in-flight ceiling = max - consumed is always exactly the window.
			expect(max - consumed).toBe(W);
		}
	});

	test("disabled when window is zero", () => {
		expect(replenishWindow(0n, 0n, 0n)).toBeNull();
	});
});
