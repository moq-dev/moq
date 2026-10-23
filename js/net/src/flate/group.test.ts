import { expect, spyOn, test } from "bun:test";
import { Producer as Group, MAX_GROUP_CACHE_BYTES } from "../group.ts";
import { hooks } from "../internal.ts";
import { Producer as Track } from "../track.ts";
import { Decoder } from "./decoder.ts";
import { Encoder } from "./encoder.ts";
import { stored } from "./group.ts";

const payload = new TextEncoder().encode("one shared compression window ".repeat(100));

test("synchronous writers store slices; plain readers share one charged inflated copy", async () => {
	const track = new Track("compressed").accept({ compress: true });
	const group = track.appendGroup();
	const first = group.mirror();
	const second = group.mirror();
	const raw = group.mirror();
	group.writeFrame({ payload });
	group.writeFrame({ payload });
	group.close();
	const stored = hooks.groupCacheBytes(group);
	expect(stored).toBeLessThan(payload.length);
	const inflating = spyOn(Decoder.prototype, "frame");
	try {
		const slice = await hooks.readGroupFrame(raw, undefined, true);
		expect(slice?.inflated).toBe(payload.length);
		expect(inflating).toHaveBeenCalledTimes(0);
		slice?.complete();
		const a = await first.readFrame();
		const b = await second.readFrame();
		expect(a?.payload).toBe(b?.payload);
		expect(a?.payload).toEqual(payload);
		expect(inflating).toHaveBeenCalledTimes(1);
		expect(hooks.groupCacheBytes(group)).toBe(stored + payload.length);
		await first.readFrame();
		await second.readFrame();
		expect(inflating).toHaveBeenCalledTimes(2);
	} finally {
		inflating.mockRestore();
		track.close();
	}
});

test("received slices are forwarded byte-for-byte and exact declarations fail all plain readers", async () => {
	const group = new Group(0);
	hooks.bindGroupCompression(group, true);
	const slice = new Encoder().frame(payload);
	hooks.writeGroupFrame(group, { payload: slice }, payload.length + 1);
	group.close();
	const raw = await hooks.readGroupFrame(group.mirror(), undefined, true);
	expect(raw?.frame.payload).toBe(slice);
	raw?.complete();
	const bad = group.mirror();
	await expect(bad.readFrame()).rejects.toThrow(/declared size/);
	expect(bad.closed.peek()).toBeInstanceOf(Error);
	await expect(group.mirror().readFrame()).rejects.toThrow(/declared size/);
});

test("declared plain bytes consume the group budget before any inflation", () => {
	const group = new Group(0);
	hooks.bindGroupCompression(group, true);
	const inflating = spyOn(Decoder.prototype, "frame");
	try {
		expect(() => hooks.writeGroupFrame(group, { payload: new Uint8Array() }, MAX_GROUP_CACHE_BYTES + 1)).toThrow();
		hooks.writeGroupFrame(group, { payload: new Uint8Array() }, MAX_GROUP_CACHE_BYTES);
		expect(() => hooks.writeGroupFrame(group, { payload: new Uint8Array() }, 1)).toThrow();
		expect(inflating).toHaveBeenCalledTimes(0);
		expect(hooks.groupCacheBytes(group)).toBe(0);
	} finally {
		inflating.mockRestore();
		group.close();
	}
});

test("empty frames preserve the window and prewritten groups bind without duplicating plain copies", async () => {
	const group = new Group(0);
	group.writeFrame({ payload });
	group.writeFrame({ payload: new Uint8Array() });
	const reader = group.mirror();
	const track = new Track("compressed").accept({ compress: true });
	track.writeGroup(group);
	group.writeFrame({ payload });
	group.close();
	for (const expected of [payload, new Uint8Array(), payload])
		expect((await reader.readFrame())?.payload).toEqual(expected);
	track.close();
});

test("a cut compressed group preserves its committed frames before returning its error", async () => {
	const track = new Track("compressed").accept({ compress: true });
	const group = track.appendGroup();
	const reader = group.mirror();
	group.writeFrame({ payload });
	const cut = new Error("cut");
	group.close(cut);
	expect((await reader.readFrame())?.payload).toEqual(payload);
	await expect(reader.readFrame()).rejects.toThrow(cut);
	track.close();
});

test("a shared plain read cannot change a later synchronous write's admission", async () => {
	const track = new Track("compressed").accept({ compress: true });
	const group = track.appendGroup();
	const reader = group.mirror();
	group.writeFrame({ payload: new Uint8Array(MAX_GROUP_CACHE_BYTES) });
	expect((await reader.readFrame())?.payload.length).toBe(MAX_GROUP_CACHE_BYTES);
	// The plain-byte budget is full, but another empty frame still fits, regardless of readers.
	expect(() => group.writeFrame({ payload: new Uint8Array() })).not.toThrow();
	group.close();
	expect((await reader.readFrame())?.payload.length).toBe(0);
	track.close();
});

test("datagrams buffered before accept share one stored and plain form", async () => {
	const track = new Track("late-info");
	const a = track.subscribe();
	const b = track.subscribe();
	track.appendDatagram(undefined, payload);
	track.accept({ compress: true });
	const first = await a.recvDatagram();
	const second = await b.recvDatagram();
	if (!first || !second) throw new Error("missing datagram");
	expect(first).toBe(second);
	expect(stored(first)?.inflated).toBe(payload.length);
	expect(first.payload).toEqual(payload);
	track.close();
});

test("a malformed compressed datagram is refused before reaching readers", () => {
	const track = new Track("compressed").accept({ compress: true });
	const reader = track.subscribe();
	const payload = new Uint8Array(100);
	const slice = new Encoder().frame(payload);
	expect(() => hooks.writeDatagram(track, { sequence: 0, payload: slice }, payload.length + 1)).toThrow(
		/declared size/,
	);
	expect(hooks.tryRecvDatagram(reader).kind).toBe("idle");
	track.close();
});

test("refresh measures stored slices without inflating a compressed group", () => {
	const track = new Track("compressed").accept({ compress: true });
	const group = track.appendGroup();
	const payload = Uint8Array.from({ length: 1024 }, (_, index) => index % 251);
	for (let index = 0; index < 3; index++) group.writeFrame({ payload });
	const inflating = spyOn(Decoder.prototype, "frame");
	try {
		expect(hooks.overRefresh(group, 1)).toBe(false);
		expect(inflating).toHaveBeenCalledTimes(0);
	} finally {
		inflating.mockRestore();
		group.close();
		track.close();
	}
});
