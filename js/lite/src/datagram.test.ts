import { expect, test } from "bun:test";
import { Datagram, MAX_DATAGRAM_PAYLOAD } from "./datagram.ts";
import { Track } from "./track.ts";

test("writeDatagram + recvDatagram round-trip", async () => {
	const track = new Track("test");
	track.writeDatagram(new Datagram(0, new Uint8Array([1, 2, 3])));
	const got = await track.recvDatagram(33);
	expect(got).toBeDefined();
	expect(got?.sequence).toBe(0);
	expect(got?.payload).toEqual(new Uint8Array([1, 2, 3]));
});

test("appendDatagram auto-increments sequence", () => {
	const track = new Track("test");
	expect(track.appendDatagram(new Uint8Array([1]))).toBe(0);
	expect(track.appendDatagram(new Uint8Array([2]))).toBe(1);
	expect(track.appendDatagram(new Uint8Array([3]))).toBe(2);
});

test("rejects oversized payload", () => {
	const track = new Track("test");
	const big = new Uint8Array(MAX_DATAGRAM_PAYLOAD + 1);
	expect(() => track.appendDatagram(big)).toThrow();
});

test("recvDatagram with maxLatency=0 + skipDatagramsToLatest skips history", async () => {
	const track = new Track("test");
	track.appendDatagram(new Uint8Array([1]));
	track.appendDatagram(new Uint8Array([2]));

	track.skipDatagramsToLatest();

	// No fresh entries remain — recvDatagram should pend. Use a short timeout race.
	const winner = await Promise.race([
		track.recvDatagram(0),
		new Promise<"timeout">((resolve) => setTimeout(() => resolve("timeout"), 30)),
	]);
	expect(winner).toBe("timeout");

	// A fresh arrival is delivered.
	track.appendDatagram(new Uint8Array([3]));
	const got = await track.recvDatagram(0);
	expect(got?.payload).toEqual(new Uint8Array([3]));
});

test("recvDatagram filters by maxLatency", async () => {
	const track = new Track("test");
	track.appendDatagram(new Uint8Array([1]));
	// Sleep long enough that the entry exceeds a tight latency budget.
	await new Promise((r) => setTimeout(r, 20));

	const winner = await Promise.race([
		track.recvDatagram(5),
		new Promise<"timeout">((resolve) => setTimeout(() => resolve("timeout"), 5)),
	]);
	// 20ms-old entry is filtered out by 5ms budget; nothing else arrives in time.
	expect(winner).toBe("timeout");
});

test("close drains datagram queue", async () => {
	const track = new Track("test");
	track.appendDatagram(new Uint8Array([1]));
	track.close();
	const got = await track.recvDatagram(33);
	expect(got).toBeUndefined();
});
