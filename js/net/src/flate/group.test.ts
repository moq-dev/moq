import { expect, spyOn, test } from "bun:test";
import { StreamCode } from "../error.ts";
import { Producer as Group } from "../group.ts";
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
	group.finish();
	const stored = hooks.groupCacheBytes(group);
	expect(stored).toBeLessThan(payload.length);
	const inflating = spyOn(Decoder.prototype, "frame");
	try {
		const slice = await hooks.readGroupFrame(raw, true);
		expect(slice?.inflated).toBe(payload.length);
		expect(inflating).toHaveBeenCalledTimes(0);
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
		track.finish();
		track.close();
	}
});

test("received slices are forwarded byte-for-byte and exact declarations fail all plain readers", async () => {
	const group = new Group(0);
	hooks.bindGroupCompression(group, true);
	const slice = new Encoder().frame(payload);
	hooks.writeGroupFrame(group, { payload: slice }, payload.length + 1);
	group.finish();
	const raw = await hooks.readGroupFrame(group.mirror(), true);
	expect(raw?.frame.payload).toBe(slice);
	const bad = group.mirror();
	await expect(bad.readFrame()).rejects.toThrow(/declared size/);
	expect(bad.closed.peek()).toBeInstanceOf(Error);
	await expect(group.mirror().readFrame()).rejects.toThrow(/declared size/);
});

for (const readPlain of [false, true]) {
	test(`compressed tails shed dictionary prefixes without retaining inflated history (plain=${readPlain})`, async () => {
		const group = new Group(0);
		hooks.bindGroupCompression(group, true);
		const raw = group.consume();
		const plain = readPlain ? group.consume() : undefined;
		const slow = group.consume();
		const encoder = new Encoder();
		const decoder = new Decoder();
		const bytes = new Uint8Array(1024 * 1024).fill(42);
		for (let index = 0; index < 100; index++) {
			const slice = encoder.frame(bytes);
			hooks.writeGroupFrame(group, { payload: slice }, bytes.length);
			const frame = await hooks.readGroupFrame(raw, true);
			expect(frame?.frame.payload).toBe(slice);
			expect(decoder.frame(slice, bytes.length)).toEqual(bytes);
			if (plain) expect((await plain.readFrame())?.payload).toEqual(bytes);
			expect(hooks.groupCacheBytes(group)).toBeLessThanOrEqual(32 * 1024 * 1024);
		}
		await expect(slow.readFrame()).rejects.toMatchObject({ code: StreamCode.Evicted });
		group.finish();
	});
}

test("empty frames preserve the window", async () => {
	const track = new Track("compressed").accept({ compress: true });
	const group = track.createGroup(0);
	group.writeFrame({ payload });
	group.writeFrame({ payload: new Uint8Array() });
	const reader = group.mirror();
	group.writeFrame({ payload });
	group.finish();
	for (const expected of [payload, new Uint8Array(), payload])
		expect((await reader.readFrame())?.payload).toEqual(expected);
	track.finish();
	track.close();
});

test("a cut compressed group preserves its committed frames before returning its error", async () => {
	const track = new Track("compressed").accept({ compress: true });
	const group = track.appendGroup();
	const reader = group.mirror();
	group.writeFrame({ payload });
	const cut = new Error("cut");
	group.abort(cut);
	expect((await reader.readFrame())?.payload).toEqual(payload);
	await expect(reader.readFrame()).rejects.toThrow(cut);
	track.finish();
	track.close();
});

test("a shared plain read cannot change a later synchronous write's admission", async () => {
	const track = new Track("compressed").accept({ compress: true });
	const group = track.appendGroup();
	const reader = group.mirror();
	group.writeFrame({ payload: new Uint8Array(32 * 1024 * 1024) });
	expect((await reader.readFrame())?.payload.length).toBe(32 * 1024 * 1024);
	// The plain-byte budget is full, but another empty frame still fits, regardless of readers.
	expect(() => group.writeFrame({ payload: new Uint8Array() })).not.toThrow();
	group.finish();
	expect((await reader.readFrame())?.payload.length).toBe(0);
	track.finish();
	track.close();
});

test("datagrams buffered before accept share one stored form, each reader its own plain", async () => {
	const track = new Track("late-info");
	const a = track.subscribe();
	const b = track.subscribe();
	track.appendDatagram(undefined, payload);
	track.accept({ compress: true });
	const first = await a.recvDatagram();
	const second = await b.recvDatagram();
	if (!first || !second) throw new Error("missing datagram");
	expect(first).not.toBe(second);
	expect(stored(first)).toBe(stored(second));
	expect(stored(first)?.inflated).toBe(payload.length);
	expect(first.payload).toEqual(payload);
	expect(second.payload).toEqual(payload);
	track.finish();
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
	track.finish();
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
		group.finish();
		track.finish();
		track.close();
	}
});
