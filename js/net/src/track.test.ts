import { expect, test } from "bun:test";
import { Producer as GroupProducer } from "./group.ts";
import { Timestamp } from "./time.ts";
import { Producer as TrackProducer } from "./track.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

test("used reflects subscriber demand and unused resolves when the last one leaves", async () => {
	const producer = new TrackProducer("test");

	// No subscribers: no demand.
	expect(producer.used.peek()).toBe(false);

	const a = producer.subscribe();
	const b = producer.subscribe();
	expect(producer.used.peek()).toBe(true);

	// Closing one of two keeps demand, so unused() stays pending.
	a.close();
	expect(producer.used.peek()).toBe(true);

	// Closing the last subscriber drops demand; unused() resolves. The consumer wire awaits this
	// to tear an idle upstream down.
	b.close();
	await producer.unused();
	expect(producer.used.peek()).toBe(false);
});

test("a producer never self-closes on zero demand: a publisher keeps serving new subscribers", async () => {
	const producer = new TrackProducer("video");

	// Demand comes and goes...
	const a = producer.subscribe();
	a.close();
	await producer.unused();

	// ...but the producer stays open (only the wire acts on `unused`, not the producer itself),
	// so a publisher writing to a track nobody is watching is unaffected.
	expect(producer.closed.peek()).toBeUndefined();

	// A later subscriber still works and replays the cache.
	producer.writeString("still here");
	const b = producer.subscribe();
	expect(await b.readString()).toBe("still here");
	expect(producer.used.peek()).toBe(true);
});

test("unused() resolves immediately when there was never any demand", async () => {
	const producer = new TrackProducer("video");
	// No subscriber was ever attached; unused() must not hang.
	await producer.unused();
	expect(producer.used.peek()).toBe(false);
});

test("used stays true across churn while at least one subscriber remains", async () => {
	const producer = new TrackProducer("video");

	const a = producer.subscribe();
	// Rapidly add and drop extra subscribers; `a` keeps demand asserted throughout.
	for (let i = 0; i < 20; i++) {
		const t = producer.subscribe();
		t.close();
		expect(producer.used.peek()).toBe(true);
	}

	a.close();
	await producer.unused();
	expect(producer.used.peek()).toBe(false);
});

test("appendDatagram shares the group sequence counter", () => {
	const producer = new TrackProducer("test");
	const ts = Timestamp.fromMillis(10);

	// Interleave groups and datagrams: they draw from one monotonic counter.
	expect(producer.appendGroup().sequence).toBe(0);
	expect(producer.appendDatagram(ts, enc.encode("a"))).toBe(1);
	expect(producer.appendGroup().sequence).toBe(2);
	expect(producer.appendDatagram(ts, enc.encode("b"))).toBe(3);
});

test("appendDatagram delivers to a subscriber", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();
	const ts = Timestamp.fromMillis(42);

	const seq = producer.appendDatagram(ts, enc.encode("hello"));
	const got = await track.recvDatagram();
	expect(got?.sequence).toBe(seq);
	expect(got?.timestamp).toBe(ts);
	expect(got && dec.decode(got.payload)).toBe("hello");
});

test("writeDatagram preserves an explicit sequence", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	// A relay forwarding an upstream datagram keeps its sequence number.
	producer.writeDatagram({ sequence: 100, timestamp: Timestamp.fromMillis(5), payload: enc.encode("x") });
	expect((await track.recvDatagram())?.sequence).toBe(100);

	// The shared counter advanced past it, so the next appended group continues from 101.
	expect(producer.appendGroup().sequence).toBe(101);
});

test("recvDatagram advances the ordered group cursor", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	producer.writeDatagram({ sequence: 5, timestamp: Timestamp.fromMillis(5), payload: enc.encode("x") });
	expect((await track.recvDatagram())?.sequence).toBe(5);

	// Ordered group reads treat lower sequences as late once a datagram used sequence 5.
	producer.writeGroup(new GroupProducer(3));
	producer.writeGroup(new GroupProducer(6));
	expect((await track.nextGroup())?.sequence).toBe(6);
});

test("appendDatagram rejects a payload over the QUIC datagram frame ceiling", () => {
	const producer = new TrackProducer("test");
	expect(() => producer.appendDatagram(Timestamp.fromMillis(0), new Uint8Array(65536))).toThrow();
});

test("subscriber options and updates are forwarded to the producer's aggregate", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ startGroup: 0 });

	// The initial options are available before the request is accepted or put on the wire.
	expect(producer.subscription.peek()).toEqual({
		priority: 0,
		ordered: false,
		latencyMax: 0,
		startGroup: 0,
		endGroup: undefined,
	});

	// The wire layer watches the producer's signal to emit SUBSCRIBE_UPDATE.
	const next = producer.subscription.changed();
	track.update({ priority: 7, ordered: true, latencyMax: 250, startGroup: 2, endGroup: 9 });
	expect(await next).toEqual({ priority: 7, ordered: true, latencyMax: 250, startGroup: 2, endGroup: 9 });
});

test("multiple subscriber options aggregate like Rust", async () => {
	const producer = new TrackProducer("test");
	const bounded = producer.subscribe({ priority: 2, ordered: true, latencyMax: 100, startGroup: 10, endGroup: 20 });
	const live = producer.subscribe({ priority: 7, ordered: false, latencyMax: 250, startGroup: 5 });

	expect(producer.subscription.peek()).toEqual({
		priority: 7,
		ordered: false,
		latencyMax: 250,
		startGroup: 5,
		endGroup: undefined,
	});

	const narrowed = producer.subscription.changed();
	live.close();
	expect(await narrowed).toEqual({
		priority: 2,
		ordered: true,
		latencyMax: 100,
		startGroup: 10,
		endGroup: 20,
	});

	const none = producer.subscription.changed();
	bounded.close();
	expect(await none).toBeUndefined();
});

test("nextGroup skips late arrivals", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	producer.writeGroup(new GroupProducer(5));

	const first = await track.nextGroup();
	expect(first?.sequence).toBe(5);

	// Late arrivals with sequence <= last returned are skipped.
	producer.writeGroup(new GroupProducer(3));
	producer.writeGroup(new GroupProducer(4));
	producer.writeGroup(new GroupProducer(7));

	const next = await track.nextGroup();
	expect(next?.sequence).toBe(7);
});

test("nextGroup returns buffered groups in sequence", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	producer.writeGroup(new GroupProducer(3));
	producer.writeGroup(new GroupProducer(5));

	expect((await track.nextGroup())?.sequence).toBe(3);
	expect((await track.nextGroup())?.sequence).toBe(5);
});

test("local cursor bounds can skip, pause, and release buffered groups", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	for (let sequence = 0; sequence < 5; sequence++) producer.writeGroup(new GroupProducer(sequence));

	expect(track.latest()).toBe(4);
	track.startAt(1);
	track.endAt(2);
	expect((await track.nextGroup())?.sequence).toBe(1);
	expect((await track.nextGroup())?.sequence).toBe(2);

	const pending = track.nextGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.startAt(4);
	track.endAt(4);
	expect((await pending)?.sequence).toBe(4);
});

test("recvGroup after nextGroup still returns late arrivals", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	producer.writeGroup(new GroupProducer(5));

	// Ordered returns seq 5, advancing its cursor.
	const ordered = await track.nextGroup();
	expect(ordered?.sequence).toBe(5);

	// recvGroup is independent of the ordered cursor: a late seq 3 still surfaces.
	producer.writeGroup(new GroupProducer(3));
	const recv = await track.recvGroup();
	expect(recv?.sequence).toBe(3);
});

test("nextGroup returns undefined when track closes", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();
	producer.close();
	expect(await track.nextGroup()).toBeUndefined();
});

// Close doesn't erase buffered frames: a group parked above the cap stays readable, so
// nextGroup must keep waiting for a cap raise rather than fake an end-of-track and lose
// the data (mirrors the Rust subscriber). Only a drained closed track reports finished.
test("a closed track still delivers a group parked above the cap once the cap is raised", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	for (let sequence = 0; sequence < 3; sequence++) {
		const group = new GroupProducer(sequence);
		group.writeString(`frame-${sequence}`);
		group.close();
		producer.writeGroup(group);
	}

	track.endAt(1);
	expect((await track.nextGroup())?.sequence).toBe(0);
	expect((await track.nextGroup())?.sequence).toBe(1);

	// Group 2 parks above the cap; a clean close must not resolve it as finished.
	const parked = track.nextGroup();
	producer.close();
	const timeout = new Promise((resolve) => setTimeout(() => resolve("pending"), 10));
	expect(await Promise.race([parked, timeout])).toBe("pending");

	// Raising the cap after close releases the buffered group, frames intact.
	track.endAt(2);
	const released = await parked;
	expect(released?.sequence).toBe(2);
	expect(await released?.readString()).toBe("frame-2");

	// Drained and closed: now it's finished.
	expect(await track.nextGroup()).toBeUndefined();
});

// The final boundary counts datagrams: they share the group sequence namespace, and a
// Rust peer feeds SUBSCRIBE_END into finish_at, so a boundary that ignored a trailing
// datagram would finalize the track before it arrives.
test("final reports one past the highest produced sequence, datagrams included", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();
	expect(track.final()).toBeUndefined();

	producer.writeGroup(new GroupProducer(0));
	producer.appendDatagram(Timestamp.fromMillis(1), enc.encode("x"));
	producer.close();
	expect(track.final()).toBe(2);
});

// 0 is the only encoding for "nothing produced"; an abort declares no boundary at all.
test("final is 0 for an empty track and undefined after an abort", async () => {
	const empty = new TrackProducer("test");
	const emptyTrack = empty.subscribe();
	empty.close();
	expect(emptyTrack.final()).toBe(0);

	const aborted = new TrackProducer("test");
	const abortedTrack = aborted.subscribe();
	aborted.close(new Error("boom"));
	expect(abortedTrack.final()).toBeUndefined();
});

test("readFrame does not livelock when a sole group finishes before the next arrives", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	// A group is appended then finished empty while the track stays open. A finished group's
	// readable() resolves immediately, so the reader must not busy-wait on it (which would starve the
	// macrotask queue and never observe the next group).
	const g0 = producer.appendGroup();
	g0.close();

	// The next group arrives via a macrotask; if the reader livelocks on microtasks it never runs.
	setTimeout(() => {
		const g1 = producer.appendGroup();
		g1.writeString("hello");
		g1.close();
		producer.close();
	}, 10);

	expect(await track.readString()).toBe("hello");
}, 2000);
