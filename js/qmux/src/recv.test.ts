import { describe, expect, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import { replenishWindow, Window } from "./credit.ts";
import { RecvStream } from "./recv.ts";

const tick = () => yieldNow();

/** A receive half over fresh windows, recording the STOP_SENDING it asks for. */
function recvStream(conn = 1000n, window = 1000n) {
	const stops: unknown[] = [];
	const windows = { conn: new Window(conn, () => {}), stream: new Window(window, () => {}) };
	const recv = new RecvStream({ conn: windows.conn, window: windows.stream, stop: (reason) => stops.push(reason) });
	return { recv, stops, ...windows };
}

describe("RecvStream", () => {
	test("delivers chunks in order and returns credit only on delivery", async () => {
		const { recv, conn, stream } = recvStream(10n, 10n);
		recv.data(0n, new Uint8Array([1]), false);
		recv.data(1n, new Uint8Array([2, 2]), false);
		recv.data(3n, new Uint8Array([3, 3, 3]), true);
		await tick();
		// A zero high-water mark keeps even one chunk from being prefetched.
		expect(conn.max).toBe(10n);

		const reader = recv.readable.getReader();
		expect((await reader.read()).value).toEqual(new Uint8Array([1]));
		expect((await reader.read()).value).toEqual(new Uint8Array([2, 2]));
		expect((await reader.read()).value).toEqual(new Uint8Array([3, 3, 3]));
		expect((await reader.read()).done).toBe(true);
		expect(recv.state).toBe("dataRead");
		// Half the connection window read, so it rose; the stream's last byte arrived, so its didn't.
		expect(conn.max).toBe(16n);
		expect(stream.max).toBe(10n);
	});

	test("a frame past either window is a flow control error", () => {
		expect(() => recvStream(10n, 5n).recv.data(0n, new Uint8Array(6), false)).toThrow("flow control error");
		expect(() => recvStream(5n, 10n).recv.data(0n, new Uint8Array(6), false)).toThrow("flow control error");
	});

	test("a frame that doesn't continue the stream is refused", () => {
		const { recv } = recvStream();
		recv.data(0n, new Uint8Array(3), false);
		expect(() => recv.data(2n, new Uint8Array(1), false)).toThrow("does not continue");
	});

	test("cancel asks for STOP_SENDING, and later bytes are charged and freed as they land", async () => {
		const { recv, stops, conn } = recvStream(100n);
		recv.data(0n, new Uint8Array(20), false);
		const reason = new Error("done");
		await recv.readable.cancel(reason);
		expect(stops).toEqual([reason]);
		expect(recv.sizeKnown).toBe(false);

		recv.data(20n, new Uint8Array(40), false);
		expect(conn.received).toBe(60n);
		// All 60 discarded, so the connection window rose.
		expect(conn.max).toBe(160n);
		recv.data(60n, new Uint8Array(), true);
		expect(recv.sizeKnown).toBe(true);
	});

	test("cancel after the final size is known sends no STOP_SENDING", async () => {
		const { recv, stops } = recvStream();
		recv.data(0n, new Uint8Array(5), true);
		await recv.readable.cancel();
		expect(stops).toEqual([]);
	});

	test("a reset charges its final-size gap and frees it with the bytes past the reliable prefix", async () => {
		const { recv, conn } = recvStream(100n);
		recv.data(0n, new Uint8Array([1, 2, 3, 4]), false);
		recv.reset(new Error("RESET_STREAM"), 60n, 2n);
		expect(conn.received).toBe(60n);
		// The gap (56) and the discarded tail (2) are free; the prefix waits for the reader.
		expect(conn.max).toBe(158n);

		const reader = recv.readable.getReader();
		expect((await reader.read()).value).toEqual(new Uint8Array([1, 2]));
		await expect(reader.read()).rejects.toThrow("RESET_STREAM");
		expect(recv.state).toBe("resetRead");
	});

	test("a reset's final size past the stream window is a flow control error", () => {
		expect(() => recvStream(100n, 10n).recv.reset(new Error("RESET_STREAM"), 11n)).toThrow("flow control error");
	});

	test("refuse discards the buffer, frees its credit, and errors the reader without STOP_SENDING", async () => {
		const { recv, stops, conn } = recvStream(100n);
		recv.data(0n, new Uint8Array(50), false);
		recv.refuse(new Error("refused"));
		expect(conn.max).toBe(150n);
		expect(stops).toEqual([]);
		await expect(recv.readable.getReader().read()).rejects.toThrow("refused");
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
