import { expect, test } from "bun:test";
import * as Broadcast from "./broadcast.ts";

test("a broadcast keeps same-name datagram and stream tracks independent", async () => {
	const broadcast = new Broadcast.Producer();
	const streams = broadcast.createTrack("audio");
	const datagrams = broadcast.createDatagrams("audio");
	const reader = broadcast.consume().datagrams("audio");
	expect(streams.appendGroup().sequence).toBe(0n);
	expect(datagrams.append(undefined, new Uint8Array([1]))).toBe(0n);
	expect(streams.appendGroup().sequence).toBe(1n);
	expect((await reader.recv())?.sequence).toBe(0n);
	reader.close();
	datagrams.finish();
	streams.finish();
	broadcast.close();
});

import * as Datagram from "./datagram.ts";
import { StreamCode, StreamError, TimestampMismatch } from "./error.ts";
import { hooks } from "./internal.ts";
import { Timescale, Timestamp } from "./time.ts";
import { wireOf } from "./wire.ts";

test("datagram readers start live, clone their cursor, and drain only an existing finish", async () => {
	const track = new Datagram.Producer("test").accept();
	track.append(undefined, new Uint8Array([0]));
	const reader = track.consume();
	track.append(undefined, new Uint8Array([1]));
	track.finish();
	const cloned = reader.clone();
	expect((await reader.recv())?.sequence).toBe(1n);
	reader.close();
	expect((await cloned.recv())?.sequence).toBe(1n);
	expect(await reader.recv()).toBeUndefined();
	expect(await track.consume().recv()).toBeUndefined();
	reader.close();
	cloned.close();
});

test("a shared datagram buffer bounds both count and bytes for stalled readers", async () => {
	for (const size of [1, 16384, 65535]) {
		const track = new Datagram.Producer("test").accept();
		const slow = track.consume();
		const fast = track.consume();
		for (let i = 0; i < 70; i++) {
			track.append(undefined, new Uint8Array(size));
			expect((await fast.recv())?.sequence).toBe(BigInt(i));
		}
		track.finish();
		const kept = Math.min(64, Math.floor(65536 / size));
		expect((await slow.recv())?.sequence).toBe(BigInt(70 - kept));
		slow.close();
		fast.close();
	}
});

test("datagram inserts preserve repeats and reordering, and append past the high water mark", async () => {
	const track = new Datagram.Producer("test").accept();
	const reader = track.consume();
	for (const sequence of [10n, 3n, 10n]) track.insert({ sequence, payload: new Uint8Array() });
	expect(track.append(undefined, new Uint8Array())).toBe(11n);
	for (const sequence of [10n, 3n, 10n, 11n]) expect((await reader.recv())?.sequence).toBe(sequence);
	track.insert({ sequence: (1n << 64n) - 1n, payload: new Uint8Array() });
	expect(() => track.append(undefined, new Uint8Array())).toThrow(RangeError);
	track.finish();
	reader.close();
});

test("refused datagram writes spend neither sequences nor buffer space", async () => {
	const track = new Datagram.Producer("test").accept({ timescale: Timescale.MILLI });
	const reader = track.consume();
	expect(() => track.append(undefined, new Uint8Array())).toThrow(TimestampMismatch);
	expect(() => track.append(Timestamp.fromMillis(1), new Uint8Array(65536))).toThrow();
	expect(track.append(Timestamp.fromMicros(1234), new Uint8Array([7]))).toBe(0n);
	expect((await reader.recv())?.timestamp).toEqual(Timestamp.fromMillis(1));
	track.finish();
	reader.close();
});

test("an abort or local close wakes datagram reads with its exact result", async () => {
	const track = new Datagram.Producer("test").accept();
	const reader = track.consume();
	track.append(undefined, new Uint8Array([1]));
	const cause = new StreamError(StreamCode.Unauthorized);
	track.abort(cause);
	await expect(reader.recv()).rejects.toBe(cause);
	const live = new Datagram.Producer("live").accept();
	const local = live.consume();
	const read = local.recv();
	local.close(cause);
	await expect(read).rejects.toBe(cause);
	reader.close();
	live.finish();
});

test("datagram priority is the maximum of live consumers and releases atomically", () => {
	const track = new Datagram.Producer("test").accept();
	const low = track.consume();
	const high = low.clone();
	low.setPriority(-3);
	high.setPriority(8);
	expect(track.priority.peek()).toBe(8);
	high.close();
	expect(track.priority.peek()).toBe(-3);
	expect(hooks.closeUnusedDatagrams(track)).toBe(false);
	low.close();
	expect(hooks.closeUnusedDatagrams(track)).toBe(true);
});

test("abandoned datagram requests leave neither a queued request nor a name claim", async () => {
	const broadcast = new Broadcast.Producer();
	const consumer = broadcast.consume();
	const pending = wireOf(consumer).requestedDatagrams();
	const left = consumer.datagrams("test");
	left.close();
	await Promise.resolve();
	const next = consumer.datagrams("test");
	const request = await pending;
	expect(request?.name).toBe("test");
	if (!request) throw new Error("missing request");
	const track = request.accept();
	track.append(undefined, new Uint8Array([1]));
	expect((await next.recv())?.payload).toEqual(new Uint8Array([1]));
	track.finish();
	next.close();
	consumer.close();
	broadcast.close();
});
