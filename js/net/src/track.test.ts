import { expect, setSystemTime, spyOn, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { ProtocolViolation, StreamCode, TimestampMismatch, TooFarBehind } from "./error.ts";
import { Producer as GroupProducer, MAX_GROUP_FRAMES } from "./group.ts";
import { hooks } from "./internal.ts";
import { textFrame } from "./mock.ts";
import { Timescale, Timestamp } from "./time.ts";
import { infoDefaults, type Subscription, Producer as TrackProducer } from "./track.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

// Matches the private idle cache window and Rust cache::DEFAULT_EXPIRY.
const CACHE_WINDOW_MS = 30_000;

/** Let every pending microtask and timer callback run, so a parked read gets a turn. */
function settle(): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, 0));
}

// A manual clock reading `initial` milliseconds. `set` moves it to an absolute time and fires
// what came due; `advance` also drains what those timers schedule.
function manualTime(initial: number) {
	const clock = new Manual();
	const restore = clock.install();
	const to = (ms: number) => Nano(Nano.fromMilli(Milli(ms)) - clock.now());
	clock.advance(to(initial));
	return {
		set: (ms: number) => clock.advance(to(ms)),
		advance: (ms: number) => clock.advanceAsync(to(ms)),
		restore,
	};
}

test("priority reads the committed info and is the midpoint before accept", () => {
	const producer = new TrackProducer("video");
	expect(producer.priority).toBe(127);
	producer.accept({ priority: 60 });
	expect(producer.priority).toBe(60);
});

test("used reflects subscriber demand and unused resolves when the last one leaves", async () => {
	const producer = new TrackProducer("test");

	// No subscribers: no demand.
	expect(producer.demand().used.peek()).toBe(false);

	const a = producer.subscribe();
	const b = producer.subscribe().ordered();
	expect(producer.demand().used.peek()).toBe(true);

	// Closing one of two keeps demand, so unused() stays pending.
	a.close();
	expect(producer.demand().used.peek()).toBe(true);

	// Closing the last subscriber drops demand; unused() resolves. The consumer wire awaits this
	// to tear an idle upstream down.
	b.close();
	await producer.demand().unused();
	expect(producer.demand().used.peek()).toBe(false);
});

test("a producer never self-closes on zero demand: a publisher keeps serving new subscribers", async () => {
	const producer = new TrackProducer("video");

	// Demand comes and goes...
	const a = producer.subscribe();
	a.close();
	await producer.demand().unused();

	// ...but the producer stays open (only the wire acts on `unused`, not the producer itself),
	// so a publisher writing to a track nobody is watching is unaffected.
	expect(producer.closed.peek()).toBeUndefined();

	// A later subscriber still works and replays the cache.
	producer.writeFrame(textFrame("still here"));
	const b = producer.subscribe().ordered();
	expect(await b.readString()).toBe("still here");
	expect(producer.demand().used.peek()).toBe(true);
});

test("unused() resolves immediately when there was never any demand", async () => {
	const producer = new TrackProducer("video");
	// No subscriber was ever attached; unused() must not hang.
	await producer.demand().unused();
	expect(producer.demand().used.peek()).toBe(false);
});

test("used stays true across churn while at least one subscriber remains", async () => {
	const producer = new TrackProducer("video");

	const a = producer.subscribe();
	// Rapidly add and drop extra subscribers; `a` keeps demand asserted throughout.
	for (let i = 0; i < 20; i++) {
		const t = producer.subscribe();
		t.close();
		expect(producer.demand().used.peek()).toBe(true);
	}

	a.close();
	await producer.demand().unused();
	expect(producer.demand().used.peek()).toBe(false);
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

// The in-range datagrams, sent between the dropped ones, show the reader is live, so the
// range is what dropped the others.
test("the group range bounds datagrams", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ groups: { start: { included: 5 } } });
	track.setGroups({ end: { excluded: 7 } });
	for (const sequence of [4, 5, 7, 6]) {
		producer.insertDatagram(sequence, Timestamp.fromMillis(0), enc.encode("x"));
	}
	producer.close();

	expect((await track.recvDatagram())?.sequence).toBe(5);
	expect((await track.recvDatagram())?.sequence).toBe(6);
	expect(await track.recvDatagram()).toBeUndefined();
});

for (const timed of [true, false]) {
	test(`late ${timed ? "timed" : "untimed"} subscribers do not replay datagrams`, async () => {
		const producer = new TrackProducer("test").accept(timed ? { timescale: Timescale.MICRO } : {});
		const timestamp = timed ? Timestamp.fromMillis(180_000) : undefined;
		producer.appendDatagram(timestamp, enc.encode("before"));
		const subscriber = producer.subscribe({ maxDelay: Milli(50) });
		const sequence = producer.appendDatagram(timestamp, enc.encode("after"));
		producer.close();
		const datagram = await subscriber.recvDatagram();
		expect(datagram?.sequence).toBe(sequence);
		expect(datagram && dec.decode(datagram.payload)).toBe("after");
		expect(await subscriber.recvDatagram()).toBeUndefined();
	});
}

test("datagram buffers drop oldest at capacity without delaying active subscribers", async () => {
	const producer = new TrackProducer("test");
	const slow = producer.subscribe();
	const fast = producer.subscribe();
	const count = 192;
	for (let sequence = 0; sequence < count; sequence++) {
		producer.appendDatagram(Timestamp.fromMillis(0), enc.encode("x"));
		expect((await fast.recvDatagram())?.sequence).toBe(sequence);
	}
	producer.close();
	for (let sequence = count - 64; sequence < count; sequence++) {
		expect((await slow.recvDatagram())?.sequence).toBe(sequence);
	}
	expect(await slow.recvDatagram()).toBeUndefined();
});

test("insertDatagram preserves an explicit sequence", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	// A relay forwarding an upstream datagram keeps its sequence number.
	producer.insertDatagram(100, Timestamp.fromMillis(5), enc.encode("x"));
	expect((await track.recvDatagram())?.sequence).toBe(100);

	// The shared counter advanced past it, so the next appended group continues from 101.
	expect(producer.appendGroup().sequence).toBe(101);
});

// Datagrams and groups are separate channels that share only a sequence namespace, so
// consuming one must not move the other's cursor.
test("recvDatagram leaves the ordered group cursor alone", async () => {
	const producer = new TrackProducer("test");
	const datagrams = producer.subscribe();
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();

	producer.insertDatagram(5, Timestamp.fromMillis(5), enc.encode("x"));
	expect((await datagrams.recvDatagram())?.sequence).toBe(5);

	producer.writeGroup(new GroupProducer(3));
	producer.writeGroup(new GroupProducer(6));
	expect((await track.nextGroup())?.sequence).toBe(3);
	expect((await track.nextGroup())?.sequence).toBe(6);
});

test("insertDatagram leaves a gap and does not rewind out of order", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();
	const ts = Timestamp.fromMillis(0);

	producer.insertDatagram(10, ts, enc.encode("high"));
	producer.insertDatagram(5, ts, enc.encode("low"));
	expect((await track.recvDatagram())?.sequence).toBe(10);
	expect((await track.recvDatagram())?.sequence).toBe(5);
	expect(producer.appendDatagram(ts, enc.encode("next"))).toBe(11);
	expect(producer.appendGroup().sequence).toBe(12);
});

test("insertDatagram duplicate is best-effort", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();
	const ts = Timestamp.fromMillis(0);

	producer.insertDatagram(3, ts, enc.encode("first"));
	producer.insertDatagram(3, ts, enc.encode("again"));
	expect(dec.decode((await track.recvDatagram())?.payload ?? new Uint8Array())).toBe("first");
	expect(dec.decode((await track.recvDatagram())?.payload ?? new Uint8Array())).toBe("again");
	expect(producer.appendDatagram(ts, enc.encode("next"))).toBe(4);
});

test("insertDatagram stale does not rewind after append", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();
	const ts = Timestamp.fromMillis(0);

	expect(producer.appendDatagram(ts, enc.encode("0"))).toBe(0);
	expect(producer.appendDatagram(ts, enc.encode("1"))).toBe(1);
	producer.insertDatagram(0, ts, enc.encode("stale"));
	expect((await track.recvDatagram())?.sequence).toBe(0);
	expect((await track.recvDatagram())?.sequence).toBe(1);
	expect((await track.recvDatagram())?.sequence).toBe(0);
	expect(producer.appendDatagram(ts, enc.encode("2"))).toBe(2);
	expect(producer.appendGroup().sequence).toBe(3);
});

test("insertDatagram after close is refused", () => {
	const producer = new TrackProducer("test");
	const ts = Timestamp.fromMillis(0);
	producer.close();
	expect(() => producer.insertDatagram(0, ts, enc.encode("x"))).toThrow("track is closed");
	expect(() => producer.appendDatagram(ts, enc.encode("x"))).toThrow("track is closed");
});

test("insertDatagram after abort is refused", () => {
	const producer = new TrackProducer("test");
	producer.close(new Error("cancel"));
	expect(() => producer.insertDatagram(0, Timestamp.fromMillis(0), enc.encode("x"))).toThrow("track is closed");
});

test("appendDatagram rejects a payload over the QUIC datagram frame ceiling", () => {
	const producer = new TrackProducer("test");
	expect(() => producer.appendDatagram(Timestamp.fromMillis(0), new Uint8Array(65536))).toThrow();
	expect(() => producer.insertDatagram(0, Timestamp.fromMillis(0), new Uint8Array(65536))).toThrow();
});

test("subscriber options and updates are forwarded to the producer's aggregate", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ groups: { start: { included: 0 } } });

	// The initial options are available before the request is accepted or put on the wire.
	expect(producer.subscription.peek()).toEqual({
		priority: 0,
		maxDelay: Milli(0),
		order: "descending",
		groups: { start: { included: 0 } },
	});

	// The wire layer watches the producer's signal to emit SUBSCRIBE_UPDATE.
	const next = producer.subscription.changed();
	track.update({
		priority: 7,
		maxDelay: Milli(250),
		order: "ascending",
		groups: { start: { included: 2 }, end: { excluded: 9 } },
	});
	expect(await next).toEqual({
		priority: 7,
		maxDelay: Milli(250),
		order: "ascending",
		groups: { start: { included: 2 }, end: { excluded: 9 } },
	});
});

test("subscriber maxAge is refused before defaults or updates take effect", () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(250) });
	const original = producer.subscription.peek();

	for (const maxAge of [Milli(500), undefined]) {
		expect(() => producer.subscribe({ maxAge } as Subscription)).toThrow("maxDelay");
		const options = { maxDelay: Milli(100), maxAge };
		expect(() => producer.subscribe(options)).toThrow(TypeError);
		expect(() => track.update(options)).toThrow("maxDelay");
		expect(producer.subscription.peek()).toEqual(original);
	}

	// Publisher retention keeps its distinct maxAge field.
	producer.accept({ maxAge: Milli(500) });
	track.close();
	producer.close();
});

// A typo from plain JS would otherwise be served as the default without a word.
test("an unknown subscriber order is refused", () => {
	const producer = new TrackProducer("test");
	expect(() => producer.subscribe({ order: "newest" } as unknown as Subscription)).toThrow(TypeError);
	expect(producer.subscription.peek()).toBeUndefined();
	producer.close();
});

test("a fractional maxDelay is rounded up before the wire sees it", async () => {
	const producer = new TrackProducer("test");

	// Subscribers derive this from measurements (a jitter estimate scaled off RTT), so a
	// fractional millisecond is expected. The wire encodes it as a varint, which throws on a
	// non-integer, and rounding down would shorten a budget the subscriber asked for.
	const track = producer.subscribe({ maxDelay: Milli(38.75) });
	expect(producer.subscription.peek()?.maxDelay).toBe(Milli(39));

	const next = producer.subscription.changed();
	track.update({ maxDelay: Milli(500.25) });
	expect((await next)?.maxDelay).toBe(Milli(501));

	// The publisher half of the budget lands on the wire through TRACK_INFO, with the same hazard.
	producer.accept({ maxAge: Milli(1000.5) });
	expect((await producer.info()).maxAge).toBe(Milli(1001));
});

test("a maxDelay or maxAge that is not a duration is refused, not rounded into one", () => {
	const producer = new TrackProducer("test");

	// The wire carries an unsigned varint, so rounding these would encode a budget the
	// caller never asked for: -0.5 would ceil to zero and silently take the live edge.
	expect(() => producer.subscribe({ maxDelay: -0.5 as Milli })).toThrow(RangeError);
	expect(() => producer.subscribe({ maxDelay: -100 as Milli })).toThrow(RangeError);
	expect(() => producer.subscribe({ maxDelay: Number.NaN as Milli })).toThrow(RangeError);
	expect(() => producer.subscribe({ maxDelay: Number.POSITIVE_INFINITY as Milli })).toThrow(RangeError);
	expect(() => producer.accept({ maxAge: -0.5 as Milli })).toThrow(RangeError);
	expect(() => producer.accept({ maxAge: Milli(Number.MAX_SAFE_INTEGER + 1) })).toThrow(RangeError);
	expect(() => producer.subscribe({ maxDelay: Milli(Number.MAX_SAFE_INTEGER + 1) })).toThrow(RangeError);
});

test("priority 255 is accepted and 256 is refused", () => {
	const producer = new TrackProducer("video");
	producer.accept({ priority: 255 });
	expect(producer.priority).toBe(255);
	expect(() => producer.accept({ priority: 256 })).toThrow(RangeError);
	expect(() => producer.accept({ priority: -1 })).toThrow(RangeError);
	expect(() => producer.accept({ priority: 1.5 })).toThrow(RangeError);
	expect(() => producer.subscribe({ priority: 256 })).toThrow(RangeError);
});

test("infoDefaults re-validates timescale and the safe maxAge bound", () => {
	expect(infoDefaults({ timescale: Timescale.MICRO }).timescale).toBe(Timescale.MICRO);
	expect(infoDefaults({ maxAge: Milli(Number.MAX_SAFE_INTEGER) }).maxAge).toBe(Milli(Number.MAX_SAFE_INTEGER));
	expect(() => infoDefaults({ timescale: 0 as Timescale })).toThrow(RangeError);
	expect(() => infoDefaults({ timescale: (Number.MAX_SAFE_INTEGER + 1) as Timescale })).toThrow(RangeError);
	expect(() => infoDefaults({ maxAge: Milli(Number.MAX_SAFE_INTEGER + 1) })).toThrow(RangeError);
});

test("multiple subscriber options aggregate like Rust", async () => {
	const producer = new TrackProducer("test");
	const bounded = producer.subscribe({
		priority: 2,
		maxDelay: Milli(100),
		order: "ascending",
		groups: { start: { included: 10 }, end: { excluded: 20 } },
	});
	const live = producer.subscribe({ priority: 7, maxDelay: Milli(250), groups: { start: { included: 5 } } });

	// One descending subscriber makes the aggregate descending, so a live reader is never held
	// behind another's backlog.
	expect(producer.subscription.peek()).toEqual({
		priority: 7,
		maxDelay: Milli(250),
		order: "descending",
		groups: { start: { included: 5 } },
	});

	const narrowed = producer.subscription.changed();
	live.close();
	expect(await narrowed).toEqual({
		priority: 2,
		maxDelay: Milli(100),
		order: "ascending",
		groups: { start: { included: 10 }, end: { excluded: 20 } },
	});

	const none = producer.subscription.changed();
	bounded.close();
	expect(await none).toBeUndefined();
});

// A resume floor above the live edge must not hide the latest group from a reader that
// joins after it: on a quiet track that group may be the only one for a long time.
test("an unfloored subscriber clears a resume floor above the live edge", () => {
	const producer = new TrackProducer("test");
	producer.subscribe({ groups: { start: { included: 4 } } });
	producer.subscribe({});
	expect(producer.subscription.peek()?.groups?.start).toBeUndefined();
});

test("the producer aggregate is clamped without changing subscriber options", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(10_000) });
	const clamped = producer.subscription.changed();

	producer.accept({ timescale: Timescale.MILLI, maxAge: Milli(2_000) });

	expect((await clamped)?.maxDelay).toBe(Milli(2_000));
	expect(track.subscription.peek()?.maxDelay).toBe(Milli(10_000));
});

test("nextGroup skips late arrivals", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe().ordered();

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
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();

	producer.writeGroup(new GroupProducer(3));
	producer.writeGroup(new GroupProducer(5));

	expect((await track.nextGroup())?.sequence).toBe(3);
	expect((await track.nextGroup())?.sequence).toBe(5);
});

// A group's age is where its content *ends*, not where it began. A long group whose tail
// is level with the live edge still owes the reader every frame in it, so judging it by
// its first timestamp would discard exactly the group being filled.
test("a long group is not stale while its tail reaches the edge", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const sub = producer.subscribe({ maxDelay: Milli(500) });

	// Group 0 spans 0..2000ms; group 1 starts at 2000ms, where group 0 ends.
	const long = producer.appendGroup();
	for (const ms of [0, 500, 1000, 1500, 2000]) {
		long.writeFrame({ payload: enc.encode(`${ms}`), timestamp: Timestamp.fromMillis(ms) });
	}
	long.close();
	producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(2000) });

	expect((await sub.recvGroup())?.sequence).toBe(0);
	expect((await sub.recvGroup())?.sequence).toBe(1);
});

test("a long group is stale once its successor falls behind", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const sub = producer.subscribe({ maxDelay: Milli(500) });

	const long = producer.appendGroup();
	for (const ms of [0, 500, 1000]) {
		long.writeFrame({ payload: enc.encode(`${ms}`), timestamp: Timestamp.fromMillis(ms) });
	}
	long.close();
	// Group 0 reaches at most 3s (where group 1 starts), a second behind the 4s edge and
	// so past the budget. A frame's duration is not on the wire, so group 0's own last
	// timestamp proves nothing about where it ends; only group 1's start bounds it.
	producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(3000) });
	producer.writeFrame({ payload: enc.encode("later"), timestamp: Timestamp.fromMillis(4000) });

	expect((await sub.recvGroup())?.sequence).toBe(1);
	expect((await sub.recvGroup())?.sequence).toBe(2);
});

// Datagrams are unordered by construction, so the sequence cursor carries them too: a
// track using both channels needs one subscription, not two.
test("the ordered handle carries datagrams", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();

	producer.insertDatagram(5, Timestamp.fromMillis(5), enc.encode("x"));
	producer.writeGroup(new GroupProducer(3));

	expect((await track.recvDatagram())?.sequence).toBe(5);
	// The datagram did not consume the group cursor.
	expect((await track.nextGroup())?.sequence).toBe(3);
});

// Both cursors write a backlog off: a buffered group is not free to deliver, since a
// consumer reading one that is already too old plays it at 1x and never catches up.
test("the latency budget skips a buffered timeline on either cursor", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const arrival = producer.subscribe();
	const ordered = producer.subscribe().ordered();

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	expect((await arrival.recvGroup())?.sequence).toBe(2);
	expect((await ordered.nextGroup())?.sequence).toBe(2);

	// The budget is the only gate: one that spans the backlog bursts it in order.
	const replay = producer.subscribe({ maxDelay: Milli(30_000) }).ordered();
	expect((await replay.nextGroup())?.sequence).toBe(0);
	expect((await replay.nextGroup())?.sequence).toBe(1);
	expect((await replay.nextGroup())?.sequence).toBe(2);
});

// An unstamped immediate successor leaves a group's reach unbounded: a later stamped
// group proves nothing about where the successor will begin, and shrinking the bound
// is the unsafe direction.
test("an unstamped immediate successor leaves reach unbounded", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	// Any budget, so only media time judges: the wall clock barely moves here.
	const track = producer.subscribe({ maxDelay: Milli(500) });

	producer.writeFrame({ payload: enc.encode("0"), timestamp: Timestamp.fromMillis(0) });
	producer.appendGroup(); // seq 1 stalls before its first frame
	producer.writeFrame({ payload: enc.encode("2"), timestamp: Timestamp.fromMillis(10_000) });
	producer.writeFrame({ payload: enc.encode("3"), timestamp: Timestamp.fromMillis(11_000) });

	// Group 1's reach is group 2's start, a second behind the edge: stale.
	// Group 0's reach is unknown until group 1 presents its first frame, so it is kept.
	expect(drain(track)).toEqual([0, 2, 3]);
});

// Groups can arrive out of sequence order; the immediate successor bounds reach even when it
// arrived last.
test("a late-arriving successor bounds a group's reach", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe({ maxDelay: Milli(500) });

	for (const [sequence, ms] of [
		[0, 0],
		[2, 3000],
		[1, 200],
	]) {
		const group = new GroupProducer(sequence);
		group.writeFrame({ payload: enc.encode(`${ms}`), timestamp: Timestamp.fromMillis(ms) });
		group.close();
		producer.writeGroup(group);
	}

	// Group 0 reaches at most 200ms, where group 1 starts, so it is past the budget.
	// Group 1 reaches 3000ms, the edge itself.
	expect((await track.recvGroup())?.sequence).toBe(1);
	expect((await track.recvGroup())?.sequence).toBe(2);
});

// The ordered frame helpers ride the same cursor, so they see the same budget: a
// backlog inside it is drained in full, and what is past it is skipped.
test("ordered frame reads follow the budget", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const ordered = producer.subscribe({ maxDelay: Milli(5000) }).ordered();
	const live = producer.subscribe().ordered();

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	expect(await ordered.readString()).toBe("0");
	expect(await ordered.readString()).toBe("1000");
	expect(await ordered.readString()).toBe("2000");

	expect(await live.readString()).toBe("2000");
});

// Interleaving nextGroup with the frame helpers is still one cursor: a nextGroup that
// moves past the group the frame helpers were draining abandons it, so a later frame
// read never runs backwards.
test("nextGroup abandons the frame helpers' group when it passes it", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();

	const zero = new GroupProducer(0);
	zero.writeFrame(textFrame("0.0"));
	zero.writeFrame(textFrame("0.1"));
	zero.close();
	producer.writeGroup(zero);

	const one = new GroupProducer(1);
	one.writeFrame(textFrame("1.0"));
	one.close();
	producer.writeGroup(one);

	const two = new GroupProducer(2);
	two.writeFrame(textFrame("2.0"));
	two.close();
	producer.writeGroup(two);

	// The frame path enters group 0; a direct nextGroup then takes group 1.
	expect((await track.readFrame())?.group).toBe(0);
	expect((await track.nextGroup())?.sequence).toBe(1);

	// The frame path must not resume group 0 behind the cursor: it continues at 2.
	const next = await track.readFrame();
	expect(next?.group).toBe(2);
	expect(dec.decode(next?.payload)).toBe("2.0");
});

// The frame helpers ride the same sequence cursor as nextGroup: a lower group arriving
// after a higher one was read is skipped, never fed to the caller backwards.
test("ordered frame reads skip a late lower-sequence group", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();

	const five = new GroupProducer(5);
	five.writeFrame(textFrame("five", Timestamp.fromMillis(5)));
	five.close();
	producer.writeGroup(five);
	expect(await track.readString()).toBe("five");

	const three = new GroupProducer(3);
	three.writeFrame(textFrame("three", Timestamp.fromMillis(3)));
	three.close();
	producer.writeGroup(three);

	const six = new GroupProducer(6);
	six.writeFrame(textFrame("six", Timestamp.fromMillis(6)));
	six.close();
	producer.writeGroup(six);

	expect(await track.readString()).toBe("six");
});

test("zero latency takes the latest group when ages are equal", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
		const track = producer.subscribe();
		producer.writeFrame(textFrame("old"));
		producer.writeFrame(textFrame("new"));

		expect((await track.recvGroup())?.sequence).toBe(1);
	} finally {
		clock.restore();
	}
});

test("latency budget admits groups within its presentation-time window", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe({ maxDelay: Milli(1500) });

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// Group 0 reaches 1s, only 1s behind the 2s edge, so a 1.5s budget still admits it.
	expect((await track.recvGroup())?.sequence).toBe(0);
	expect((await track.recvGroup())?.sequence).toBe(1);
	expect((await track.recvGroup())?.sequence).toBe(2);
});

test("a late lower group within the budget is delivered", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(30_000) });
	for (const [sequence, timestamp] of [
		[5, 1000],
		[6, 2000],
		[7, 3000],
	]) {
		const group = new GroupProducer(sequence);
		producer.writeGroup(group);
		group.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
		group.close();
	}

	const track = producer.subscribe({ maxDelay: Milli(5000) });
	expect((await track.recvGroup())?.sequence).toBe(5);
	expect((await track.recvGroup())?.sequence).toBe(6);
	expect((await track.recvGroup())?.sequence).toBe(7);

	// Arriving below everything already delivered is not what makes content stale: the
	// budget is the only gate, and this straggler's timestamp is within it. A consumer
	// that needs sequence order reorders (or drops) it itself.
	const late = new GroupProducer(4);
	producer.writeGroup(late);
	late.writeFrame({ payload: enc.encode("late"), timestamp: Timestamp.fromMillis(500) });
	late.close();

	expect((await track.recvGroup())?.sequence).toBe(4);
	producer.close();
	expect(await track.recvGroup()).toBeUndefined();
});

test("a named start is a floor, not a request", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(30_000) });
	for (const timestamp of [0, 1000, 2000, 3000, 4000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// The budget is the only thing that asks for data; a named start only bounds how far
	// back it may reach.
	const floored = producer.subscribe({ groups: { start: { included: 3 } }, maxDelay: Milli(60_000) });
	expect((await floored.recvGroup())?.sequence).toBe(3);
	expect((await floored.recvGroup())?.sequence).toBe(4);

	// Naming group 1 at real time still delivers the live edge alone: the zero budget
	// calls everything older stale.
	const named = producer.subscribe({ groups: { start: { included: 1 } } });
	expect((await named.recvGroup())?.sequence).toBe(4);
});

test("the ordered cursor sheds a stale backlog", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(30_000) });
	for (const timestamp of [0, 1000, 2000, 3000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// The whole backlog is already buffered, so no read ever blocks on it: without the
	// budget here the cursor would replay four seconds of history at 1x and stay behind.
	const live = producer.subscribe().ordered();
	expect((await live.nextGroup())?.sequence).toBe(3);

	// Only what is provably too old goes. Group 0 reaches 1s, a full 2s behind the 3s
	// edge; group 1 reaches 2s and could still present inside a 1.5s budget.
	const bounded = producer.subscribe({ maxDelay: Milli(1500) }).ordered();
	expect((await bounded.nextGroup())?.sequence).toBe(1);
	expect((await bounded.nextGroup())?.sequence).toBe(2);
	expect((await bounded.nextGroup())?.sequence).toBe(3);

	// A budget spanning the history still bursts it in full, gap-free.
	const replay = producer.subscribe({ maxDelay: Milli(30_000) }).ordered();
	for (const sequence of [0, 1, 2, 3]) {
		expect((await replay.nextGroup())?.sequence).toBe(sequence);
	}
});

test("the budget is clamped to the publisher's window", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(1500) });
	const track = producer.subscribe({ maxDelay: Milli(60_000) });

	// Spaced so group 0's reach (2s) sits 2s behind the edge (4s): outside the
	// publisher's 1.5s window, inside the subscriber's requested minute.
	for (const timestamp of [0, 2000, 4000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// Asking to tolerate a minute cannot reach back further than the publisher keeps a group
	// live, the same clamp delivery applies.
	expect((await track.recvGroup())?.sequence).toBe(1);
});

// The stalled group needs no timestamp of its own: its reach is where its stamped
// successor begins, which the zero budget already puts out of range.
test("a stamped successor expires a group stalled before its first frame", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
		const track = producer.subscribe();
		producer.appendGroup(); // seq 0 has no timestamp

		clock.set(11_000);
		producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1000) });
		producer.close();

		expect((await track.recvGroup())?.sequence).toBe(1);
		expect(track.recvGroup()).resolves.toBeUndefined();
	} finally {
		clock.restore();
	}
});

test("real time reads a live stream without truncating it", async () => {
	// 2s GOPs produced one at a time and read as they arrive, at the default (zero)
	// budget. Taking the live edge must not shorten the group the reader is already on,
	// so every frame of every group arrives.
	//
	// The reader is parked at its group's end when the next one opens, because a group's
	// close and the next group's first frame are separate events. A zero budget gives up
	// there, and the group fails with Old since its producer never closed it, but only
	// after the reader took every frame.
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(60_000) });
	const track = producer.subscribe();

	let open = producer.appendGroup();
	open.writeFrame({ payload: enc.encode("key"), timestamp: Timestamp.fromMillis(0) });
	open.writeFrame({ payload: enc.encode("tail"), timestamp: Timestamp.fromMillis(1900) });

	let reading = await track.recvGroup();
	const read: Array<[number, number]> = [];

	for (let n = 1; n < 5; n++) {
		if (!reading) throw new Error("missing live group");
		const sequence = reading.sequence;

		let frames = 0;
		while (reading.tryReadFrame()) frames++;
		read.push([sequence, frames]);

		// Parked at the end of the current group, with no close yet. Settled now, since it
		// fails before anything awaits it.
		const end = reading.readFrame().then(
			() => undefined,
			(err: unknown) => err,
		);

		// The next keyframe opens its group. The verdict is taken in this window, before
		// the previous group's close lands: give the parked read a real turn to run.
		const next = producer.appendGroup();
		next.writeFrame({ payload: enc.encode("key"), timestamp: Timestamp.fromMillis(n * 2000) });
		await settle();
		open.close();
		next.writeFrame({ payload: enc.encode("tail"), timestamp: Timestamp.fromMillis(n * 2000 + 1900) });

		expect(await end).toMatchObject({ code: StreamCode.Old });

		reading = await track.recvGroup();
		open = next;
	}

	expect(read).toEqual([
		[0, 2],
		[1, 2],
		[2, 2],
		[3, 2],
	]);
});

test("a budget is measured from the reader's position", async () => {
	// A 2s GOP with a 1s budget: the reader has drained to 1900ms when the next group
	// opens at 2000ms, so it is 100ms behind the live edge and well inside what it asked
	// for. A straggling frame of the old group must still reach it. Measuring from the
	// group's first frame instead makes the drift 2000ms, so a budget shorter than one
	// GOP would drop the tail of every GOP.
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(60_000) });
	const track = producer.subscribe({ maxDelay: Milli(1000) });

	const open = producer.appendGroup();
	open.writeFrame({ payload: enc.encode("key"), timestamp: Timestamp.fromMillis(0) });
	open.writeFrame({ payload: enc.encode("tail"), timestamp: Timestamp.fromMillis(1900) });

	const reading = await track.recvGroup();
	if (!reading) throw new Error("missing group");
	expect(reading.tryReadFrame()).toBeDefined();
	expect(reading.tryReadFrame()).toBeDefined();

	// Parked at 1900ms, then the next GOP opens 100ms ahead of it.
	const late = reading.readFrame();
	const next = producer.appendGroup();
	next.writeFrame({ payload: enc.encode("key"), timestamp: Timestamp.fromMillis(2000) });
	await settle();

	// A straggler from the old group, still inside the budget.
	open.writeFrame({ payload: enc.encode("late"), timestamp: Timestamp.fromMillis(1950) });
	const frame = await late;
	expect(frame?.timestamp?.asMillis()).toBe(1950);
});

test("a handed-out group still expires while its first frame is stalled", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
		const track = producer.subscribe();
		producer.appendGroup();

		const stalled = await track.recvGroup();
		expect(stalled?.sequence).toBe(0);
		if (!stalled) throw new Error("missing stalled group");
		const pending = stalled.readFrame();
		const closed = stalled.closed;

		clock.set(11_000);
		producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1000) });

		// The producer never closed it, so even an empty prefix is not the group.
		await expect(pending).rejects.toMatchObject({ code: StreamCode.Old });
		expect(await closed).toMatchObject({ code: StreamCode.Old });
	} finally {
		clock.restore();
	}
});

test("committing track info wakes a group newly outside the retention window", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(10_000) });
	const source = producer.appendGroup();
	source.writeFrame({ payload: enc.encode("old"), timestamp: Timestamp.fromMillis(0) });

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect((await group.readFrame())?.timestamp?.asMillis()).toBe(0);
	const waiting = group.readFrame();

	producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1_000) });
	// A group beyond the edge, so group 0's reach (1s) is provably behind it: a group is
	// bounded by where its successor begins, so the successor alone never convicts it.
	producer.writeFrame({ payload: enc.encode("later"), timestamp: Timestamp.fromMillis(2_000) });
	await settle();
	producer.accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });

	await expect(waiting).rejects.toMatchObject({ code: StreamCode.Old });
});

test("a handed-out frame cancels its in-flight operation when it expires", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe();
	producer.writeFrame(textFrame("old"));

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect(await group.readString()).toBe("old");

	let release!: () => void;
	const operation = new Promise<void>((resolve) => {
		release = resolve;
	});
	const guarded = hooks.guardGroup(group, () => operation);
	producer.writeFrame(textFrame("new"));

	await expect(guarded).rejects.toThrow("max delay budget");
	release();
});

test("a guarded write keeps the position of the frame removed from the buffer", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });
	const track = producer.subscribe({ maxDelay: Milli(100) });
	const source = producer.appendGroup();
	source.writeFrame({ payload: enc.encode("old"), timestamp: Timestamp.fromMillis(0) });
	source.writeFrame({ payload: enc.encode("future"), timestamp: Timestamp.fromMillis(10_000) });

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	const read = await hooks.readGroupFrame(group);
	if (!read) throw new Error("missing frame");

	let release!: () => void;
	const operation = new Promise<void>((resolve) => {
		release = resolve;
	});
	const guarded = hooks.guardGroup(group, () => operation);

	producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1_000) });
	// A group beyond the edge, so group 0's reach (1s) is provably behind it: a group is
	// bounded by where its successor begins, so the successor alone never convicts it.
	producer.writeFrame({ payload: enc.encode("later"), timestamp: Timestamp.fromMillis(2_000) });
	await expect(guarded).rejects.toThrow("max delay budget");
	read.complete();
	release();
});

test("clean source closure stays provisional while a frame write can expire", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });
	const track = producer.subscribe({ maxDelay: Milli(100) });
	const source = producer.appendGroup();
	source.writeFrame({ payload: enc.encode("old"), timestamp: Timestamp.fromMillis(0) });
	source.close();

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	const closed = group.closed;
	expect(closed.peek()).toBeUndefined();

	const read = await hooks.readGroupFrame(group);
	if (!read) throw new Error("missing frame");
	let release!: () => void;
	const operation = new Promise<void>((resolve) => {
		release = resolve;
	});
	const guarded = hooks.guardGroup(group, () => operation);

	const edge = producer.appendGroup();
	edge.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1_000) });
	// See above: a group is bounded by where its successor begins, so convicting group 0
	// needs a group beyond that successor.
	producer.writeFrame({ payload: enc.encode("later"), timestamp: Timestamp.fromMillis(2_000) });
	await expect(guarded).rejects.toThrow("max delay budget");
	expect(await closed).toBeInstanceOf(Error);

	read.complete();
	release();
});

test("a drained group finishes cleanly after the live edge advances", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe();
	producer.writeFrame(textFrame("old"));

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect(await group.readString()).toBe("old");

	producer.writeFrame(textFrame("new"));

	expect(await group.readFrame()).toBeUndefined();
	expect(group.done).toBe(true);
});

// A stalled track keeps its newest group and gives up the ones before it by the wall clock:
// maxAge measures how long ago each group's successor arrived as well as media time, so a
// publisher that stops writing cannot pin its superseded groups.
test("a stalled track keeps only its newest group", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(1000) });
	try {
		for (const millis of [0, 10, 20]) {
			producer.writeFrame({ payload: enc.encode(`${millis}`), timestamp: Timestamp.fromMillis(millis) });
		}
		expect(drain(producer.subscribe({ maxDelay: Milli(1000) }))).toEqual([0, 1, 2]);

		// Nothing is written again: media time stands still.
		clock.set(12_000);
		expect(drain(producer.subscribe({ maxDelay: Milli(1000) }))).toEqual([2]);
	} finally {
		producer.close();
		clock.restore();
	}
});

// A peer whose timestamps stop advancing never ages a group out on media time. The wall
// clock still does.
test("frozen timestamps still age out by wall clock", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const frame = () => producer.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(0) });
	try {
		frame();
		clock.set(10_500);
		frame();
		clock.set(11_300);
		frame();

		// Group 0's successor arrived 800ms ago, group 1's just now.
		expect(drain(producer.subscribe({ maxDelay: Milli(1000) }))).toEqual([0, 1, 2]);
		clock.set(11_500);
		expect(drain(producer.subscribe({ maxDelay: Milli(1000) }))).toEqual([1, 2]);
		clock.set(12_500);
		expect(drain(producer.subscribe({ maxDelay: Milli(1000) }))).toEqual([2]);
	} finally {
		producer.close();
		clock.restore();
	}
});

// Arrival is per track: a group joining a second track later doesn't restart the first
// track's clock.
test("a group shared by two tracks ages each from its own arrival", () => {
	const clock = manualTime(10_000);
	const first = new TrackProducer("first").accept({});
	const second = new TrackProducer("second").accept({});
	try {
		first.writeFrame({ payload: enc.encode("0") });
		const shared = new GroupProducer(1);
		shared.writeFrame({ payload: enc.encode("1") });
		shared.close();
		first.writeGroup(shared);
		clock.set(11_000);
		second.writeGroup(shared);

		// Group 0's successor joined the first track a full budget ago.
		expect(drain(first.subscribe({ maxDelay: Milli(1000) }))).toEqual([1]);
	} finally {
		first.close();
		second.close();
		clock.restore();
	}
});

// Groups that arrive at once are judged by media time, which convicts them before the wall
// clock can.
test("a startup burst ages out by media time", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	try {
		for (let second = 0; second < 5; second++) {
			producer.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(second * 1000) });
		}
		// Group N reaches N + 1 seconds against the 4s edge: groups 0 and 1 are 3s and 2s
		// behind, past 1.5s. Their successors arrived just now.
		expect(drain(producer.subscribe({ maxDelay: Milli(1500) }))).toEqual([2, 3, 4]);
	} finally {
		producer.close();
		clock.restore();
	}
});

// A read parked on a superseded group is released by the wall clock alone: nothing is
// written after its successor arrives, so no write could wake it.
test("a parked read wakes at its wall deadline", async () => {
	for (const timescale of [undefined, Timescale.MILLI]) {
		const clock = manualTime(10_000);
		const producer = new TrackProducer("test").accept({ timescale });
		try {
			const track = producer.subscribe({ maxDelay: Milli(1000) });
			const held = producer.appendGroup();
			held.writeFrame({ payload: enc.encode("x"), timestamp: timescale && Timestamp.fromMillis(0) });
			const group = track.tryRecvGroup();
			if (!group) throw new Error("missing group");
			expect(await group.readString()).toBe("x");

			// The successor never presents a frame, so media time never bounds the held group.
			producer.appendGroup();
			let woken = false;
			const read = group.readFrame().then(
				() => undefined,
				(err: unknown) => {
					woken = true;
					return err;
				},
			);
			await Promise.resolve();

			await clock.advance(10_900);
			expect(woken).toBe(false);
			await clock.advance(11_000);
			expect(await read).toMatchObject({ code: StreamCode.Old });
		} finally {
			producer.close();
			clock.restore();
		}
	}
});

// Empty groups are handed out like any other and retired by the wall clock like an untimed
// one, whether they finished without a frame or are still open with none. A publisher
// flooding a run of them cannot pin the group before the run either.
test("a run of empty groups ages out by wall clock", () => {
	const RUN = 1000;
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	try {
		const track = producer.subscribe({ maxDelay: Milli(1000) });
		producer.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(0) });
		for (let sequence = 1; sequence <= RUN; sequence++) {
			const group = producer.appendGroup();
			if (sequence % 2 === 0) group.close();
		}
		producer.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(10) });
		expect(drain(producer.subscribe({ maxDelay: Milli(1000) }))).toHaveLength(RUN + 2);

		clock.set(11_000);
		expect(drain(track)).toEqual([RUN + 1]);
		expect(drain(producer.subscribe({ maxDelay: Milli(1000) }))).toEqual([RUN + 1]);
	} finally {
		producer.close();
		clock.restore();
	}
});

test("a live track retains its newest closed group through idle expiry", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });
	try {
		// A late older group must not replace the protected newest snapshot.
		for (const sequence of [2, 1]) {
			const group = new GroupProducer(sequence);
			group.writeFrame({ payload: enc.encode("snapshot"), timestamp: Timestamp.fromMillis(0) });
			group.close();
			producer.writeGroup(group);
		}
		await clock.advance(10_000 + CACHE_WINDOW_MS + 200);
		const replay = producer.subscribe({ maxDelay: Milli(100) });
		expect(replay.tryRecvGroup()?.sequence).toBe(2);
		expect(replay.tryRecvGroup()).toBeUndefined();
		replay.close();
	} finally {
		producer.close();
		clock.restore();
	}
});

// Retention is the wall-clock half of the split: it reclaims idle content on its own
// schedule, and a reader that loses unread frames to it sees a gap. The subscription
// budget is timestamp-only and says nothing here (the empty successor has no timestamp
// to bound the held group with).
test("retention eviction surfaces as a gap for a handed-out group", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });
		const track = producer.subscribe({ maxDelay: Milli(100) });
		const source = producer.appendGroup();
		source.writeFrame(textFrame("first"));
		source.writeFrame(textFrame("tail"));
		source.close();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(new TextDecoder().decode((await group.readFrame())?.payload)).toBe("first");

		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.appendGroup();

		await expect(group.readFrame()).rejects.toThrow(TooFarBehind);
	} finally {
		clock.restore();
	}
});

test("idle expiry reclaims an ended track's held mirror", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });
		const track = producer.subscribe({ maxDelay: Milli(100) });
		const source = producer.appendGroup();
		source.writeFrame(textFrame("first"));
		source.writeFrame(textFrame("tail"));
		source.close();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(new TextDecoder().decode((await group.readFrame())?.payload)).toBe("first");

		producer.close();
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe({ maxDelay: Milli(100) });

		await expect(group.readFrame()).rejects.toBeInstanceOf(TooFarBehind);
	} finally {
		clock.restore();
	}
});

test("retention reclaims a group the publisher abandoned open", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });
		const track = producer.subscribe({ maxDelay: Milli(100) });
		const stalled = producer.appendGroup();
		stalled.writeFrame(textFrame("first"));
		// A successor, so the stalled group is not the live edge the publisher is
		// still filling. It is never closed: the publisher simply walked away.
		producer.appendGroup();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("first");

		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe({ maxDelay: Milli(100) });

		// A gap, not a clean finish: the publisher never ended this group.
		await expect(group.readFrame()).rejects.toBeInstanceOf(TooFarBehind);
	} finally {
		clock.restore();
	}
});

test("an idle live edge ages out once a newer group arrives", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });
		const track = producer.subscribe({ maxDelay: Milli(100) });
		const edge = producer.appendGroup();
		edge.writeFrame(textFrame("first"));

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("first");

		// A prune while it is still the live edge keeps it, and finds nothing else to age out.
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe({ maxDelay: Milli(100) });

		// A successor ends the exemption, and the group is long past the window, so the
		// write evicts it rather than a later wakeup.
		clock.set(10_000 + CACHE_WINDOW_MS + 4_000);
		producer.appendGroup();
		const read = group.readFrame().then(
			() => "clean end",
			(err: unknown) => err,
		);
		expect(await Promise.race([read, settle().then(() => "still parked")])).toBeInstanceOf(TooFarBehind);
	} finally {
		clock.restore();
	}
});

test("an abandoned open group ages out with no further write", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	try {
		// A budget past the cache window, so the eviction rather than the budget ends it.
		const track = producer.subscribe({ maxDelay: Milli(2 * CACHE_WINDOW_MS) });
		const stalled = producer.appendGroup();
		stalled.writeFrame(textFrame("first"));
		producer.appendGroup();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("first");
		const read = group.readFrame().catch((err: unknown) => err);
		await clock.advance(10_000 + CACHE_WINDOW_MS + 1);
		expect(await read).toBeInstanceOf(TooFarBehind);
	} finally {
		producer.close();
		clock.restore();
	}
});

test("publisher maxAge does not change the idle cache deadline", async () => {
	for (const maxAge of [Milli(30), Milli(2 ** 31 + 10_000)]) {
		const clock = manualTime(10_000);
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge });
		try {
			const idle = producer.appendGroup();
			idle.writeFrame(textFrame("cached"));
			producer.appendGroup();
			// Eviction ends the idle open group a cache window after its last write, give or take
			// the one slice a scan can lag.
			await clock.advance(10_000 + CACHE_WINDOW_MS - 1);
			expect(idle.closed.peek()).toBeUndefined();
			await clock.advance(10_000 + CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8);
			expect(idle.closed.peek()).toBeInstanceOf(TooFarBehind);
		} finally {
			producer.close();
			clock.restore();
		}
	}
});

test("retention pruning preserves clean EOF for a drained mirror", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });
		const track = producer.subscribe({ maxDelay: Milli(100) });
		const source = producer.appendGroup();
		source.writeFrame(textFrame("only"));
		source.close();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("only");
		expect(await group.readFrame()).toBeUndefined();

		producer.close();
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe({ maxDelay: Milli(100) });

		expect(await group.readFrame()).toBeUndefined();
		expect(await group.closed).toBeNull();
	} finally {
		clock.restore();
	}
});

test("system clock changes do not affect the latency budget", async () => {
	const clock = manualTime(10_000);
	setSystemTime(new Date(10_000));
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });
		const track = producer.subscribe({ maxDelay: Milli(100) });
		producer.writeFrame(textFrame("old"));

		setSystemTime(new Date(20_000));
		producer.writeFrame(textFrame("new"));

		expect((await track.recvGroup())?.sequence).toBe(0);
		expect((await track.recvGroup())?.sequence).toBe(1);
	} finally {
		setSystemTime();
		clock.restore();
	}
});

test("frame readiness cancels the losing latency waiter", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe({ maxDelay: Milli(5000) });
	const source = producer.appendGroup();
	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");

	for (let i = 0; i < 110; i++) {
		const pending = group.readFrame();
		source.writeFrame(textFrame(`${i}`));
		expect(new TextDecoder().decode((await pending)?.payload)).toBe(`${i}`);
	}
});

test("latency budget retains a consumed live-edge anchor", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe();

	const edge = new GroupProducer(2);
	edge.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(2000) });
	edge.close();
	producer.writeGroup(edge);
	expect((await track.recvGroup())?.sequence).toBe(2);

	const late = new GroupProducer(1);
	late.writeFrame({ payload: enc.encode("late"), timestamp: Timestamp.fromMillis(0) });
	late.close();
	producer.writeGroup(late);
	producer.close();

	expect(track.recvGroup()).resolves.toBeUndefined();
});

test("an aborted live-edge anchor does not make older content stale", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe();

	const edge = new GroupProducer(2);
	edge.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(2000) });
	edge.close(new Error("aborted"));
	producer.writeGroup(edge);

	const older = new GroupProducer(1);
	older.writeFrame({ payload: enc.encode("older"), timestamp: Timestamp.fromMillis(0) });
	older.close();
	producer.writeGroup(older);

	expect((await track.recvGroup())?.sequence).toBe(1);
	track.close();
});

test("setGroups with an excluded end of 0 is the empty range and does not ride another subscriber's demand", async () => {
	const producer = new TrackProducer("test");
	const everything = producer.subscribe({ maxDelay: Milli(5000) });
	const empty = producer.subscribe({ maxDelay: Milli(5000), groups: { end: { excluded: 0 } } });
	empty.setGroups({ end: { excluded: 0 } });

	for (let sequence = 0; sequence < 3; sequence++) producer.writeGroup(new GroupProducer(sequence));

	expect((await everything.recvGroup())?.sequence).toBe(0);
	const pending = empty.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	empty.setGroups({ end: { excluded: 2 } });
	expect((await pending)?.sequence).toBe(0);
	expect((await empty.recvGroup())?.sequence).toBe(1);

	empty.setGroups({});
	expect((await empty.recvGroup())?.sequence).toBe(2);
});

test("setGroups caps the live edge used by the latency budget", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe();
	track.setGroups({ end: { excluded: 2 } });

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	expect((await track.recvGroup())?.sequence).toBe(1);
	track.setGroups({});
	expect((await track.recvGroup())?.sequence).toBe(2);
});

// The frame helpers ride the group cursor, so its bounds apply to them too: a group
// above the cap parks (surviving a clean close) until the cap admits it.
test("setGroups caps frame-level reads like the group cursor", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();
	track.setGroups({ end: { excluded: 1 } });

	producer.writeFrame({ payload: enc.encode("zero"), timestamp: Timestamp.fromMillis(0) });
	producer.writeFrame({ payload: enc.encode("one"), timestamp: Timestamp.fromMillis(1) });
	producer.close();

	expect((await track.readFrame())?.group).toBe(0);

	// Group 1 parks above the cap; a clean close must not resolve it as finished.
	const parked = track.readFrame();
	const timeout = new Promise((resolve) => setTimeout(() => resolve("pending"), 10));
	expect(await Promise.race([parked, timeout])).toBe("pending");

	// Raising the cap releases the parked group, frames intact.
	track.setGroups({ end: { excluded: 2 } });
	expect((await parked)?.group).toBe(1);
	expect(await track.readFrame()).toBeUndefined();
});

test("local cursor bounds can skip, pause, and release buffered groups", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();

	for (let sequence = 0; sequence < 5; sequence++) producer.writeGroup(new GroupProducer(sequence));

	track.setGroups({ start: { included: 1 } });
	track.setGroups({ end: { excluded: 3 } });
	expect((await track.nextGroup())?.sequence).toBe(1);
	expect((await track.nextGroup())?.sequence).toBe(2);

	const pending = track.nextGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.setGroups({ start: { included: 4 } });
	track.setGroups({ end: { excluded: 5 } });
	expect((await pending)?.sequence).toBe(4);
});

// A relay can ingest back-to-back groups micro-reordered (the upstream leg sends
// newest-first). The older group is cached and in demand, so serving must still
// deliver it; a sequence cursor would skip it permanently.
test("recvGroup serves a late arrival after a newer group", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) });

	producer.writeGroup(new GroupProducer(2));
	expect((await track.recvGroup())?.sequence).toBe(2);

	// Group 1 lands after group 2 was already served.
	producer.writeGroup(new GroupProducer(1));
	expect((await track.recvGroup())?.sequence).toBe(1);

	// Staleness is the latency window's job, not arrival order's: the track
	// still finishes normally afterward.
	producer.close();
	expect(await track.recvGroup()).toBeUndefined();
});

// recvGroup honors the setGroups cap by parking, like nextGroup: beyond-cap groups are
// held, not dropped, and a raised cap re-offers them, even after a clean close.
test("setGroups parks recvGroup beyond the cap and a raised cap re-offers", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) });

	for (let sequence = 0; sequence < 3; sequence++) producer.writeGroup(new GroupProducer(sequence));

	track.setGroups({ end: { excluded: 2 } });
	expect((await track.recvGroup())?.sequence).toBe(0);
	expect((await track.recvGroup())?.sequence).toBe(1);

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	// A clean close keeps the parked group claimable: the cap may still rise.
	producer.close();
	expect(await Promise.race([pending, new Promise((resolve) => setTimeout(() => resolve("parked"), 10))])).toBe(
		"parked",
	);

	track.setGroups({});
	expect((await pending)?.sequence).toBe(2);
	expect(await track.recvGroup()).toBeUndefined();
});

// A group beyond the cap must not block in-range groups that arrive behind it:
// a relay can ingest a burst micro-reordered (newest first).
test("recvGroup serves in-range groups that arrive behind a capped one", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) });

	track.setGroups({ end: { excluded: 2 } });

	// Reordered burst: the beyond-cap group arrives first.
	producer.writeGroup(new GroupProducer(2));
	producer.writeGroup(new GroupProducer(0));
	producer.writeGroup(new GroupProducer(1));

	expect((await track.recvGroup())?.sequence).toBe(0);
	expect((await track.recvGroup())?.sequence).toBe(1);

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.setGroups({ end: { excluded: 3 } });
	expect((await pending)?.sequence).toBe(2);
});

// A raised setGroups drops parked groups it overtook instead of re-offering them
// once the cap rises.
test("setGroups drops groups recvGroup parked at the cap", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	track.setGroups({ end: { excluded: 1 } });
	producer.writeGroup(new GroupProducer(1));
	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.setGroups({ start: { included: 2 } });
	track.setGroups({});
	producer.writeGroup(new GroupProducer(2));

	// The overtaken parked group is dropped, not re-offered.
	expect((await pending)?.sequence).toBe(2);
});

// A live duplicate would fan out to every subscriber twice (recvGroup has no sequence
// cursor to hide it), so writeGroup rejects it like Rust's claim_sequence. An aborted
// incarnation is evicted so a fresh group can serve the sequence again.
test("writeGroup rejects a duplicate live sequence", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	const first = new GroupProducer(7);
	first.writeFrame(textFrame("first"));
	first.close();
	producer.writeGroup(first);

	// A peer re-sending a live sequence is rejected, not fanned out again.
	expect(() => producer.writeGroup(new GroupProducer(7))).toThrow("duplicate group");
	expect((await track.recvGroup())?.sequence).toBe(7);

	// An aborted incarnation is replaceable: the retry serves the sequence fresh.
	const aborted = new GroupProducer(8);
	producer.writeGroup(aborted);
	aborted.close(new Error("upstream reset"));

	const retry = new GroupProducer(8);
	retry.writeFrame(textFrame("retry"));
	retry.close();
	producer.writeGroup(retry);

	const got = await track.recvGroup();
	expect(got?.sequence).toBe(8);
	expect(await got?.readString()).toBe("retry");
});

// A group parked at the cap outlives a clean producer close on purpose, so the
// subscriber leaving is what must release it: close() drops the parked group and
// settles the pending read instead of leaving it hanging forever.
test("closing the subscriber releases a recvGroup parked after a clean close", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	track.setGroups({ end: { excluded: 1 } });
	producer.writeGroup(new GroupProducer(1));

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	producer.close();
	track.close();
	expect(await pending).toBeUndefined();
});

// Same, but with the producer still live: the first close() must also clear the
// buffer, or the parked group re-parks on wake and the read hangs forever.
test("closing the subscriber releases a recvGroup parked while the producer is live", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	track.setGroups({ end: { excluded: 1 } });
	producer.writeGroup(new GroupProducer(1));

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.close();
	expect(await pending).toBeUndefined();
});

test("the ordered and arrival cursors are independent", async () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ maxDelay: Milli(5000) }).ordered();
	const arrival = producer.subscribe({ maxDelay: Milli(5000) });

	producer.writeGroup(new GroupProducer(5));
	expect((await ordered.nextGroup())?.sequence).toBe(5);

	// A late seq 3 is skipped by the ordered cursor, which has already passed it, but the
	// arrival cursor has both buffered and still delivers each exactly once.
	producer.writeGroup(new GroupProducer(3));
	expect((await arrival.recvGroup())?.sequence).toBe(3);
	expect((await arrival.recvGroup())?.sequence).toBe(5);
});

// Both cursors draw from one buffer, so interleaving them produces a stream in neither
// order. The first group read commits the subscription; the other order is refused.
test("the first group read commits the cursor and refuses the other order", async () => {
	const producer = new TrackProducer("test");

	// An arrival read commits to arrival order; ordered() is refused afterwards.
	const arrival = producer.subscribe({ maxDelay: Milli(5000) });
	producer.writeGroup(new GroupProducer(5));
	expect((await arrival.recvGroup())?.sequence).toBe(5);
	expect(() => arrival.ordered()).toThrow("arrival order");

	// The commitment refuses the other cursor without poisoning this one: a late
	// lower sequence still flows in arrival order.
	producer.writeGroup(new GroupProducer(3));
	expect((await arrival.recvGroup())?.sequence).toBe(3);

	// The other direction: ordered() takes the subscription and arrival reads throw.
	const inert = producer.subscribe({ maxDelay: Milli(5000) });
	const ordered = inert.ordered();
	expect(inert.recvGroup()).rejects.toThrow("sequence order");
	expect(() => inert.ordered()).toThrow("sequence order");
	expect((await ordered.nextGroup())?.sequence).toBe(3);
	expect((await ordered.nextGroup())?.sequence).toBe(5);
});

// Datagrams are a separate cursor on either handle, so reading them must not commit
// the subscription to a group order.
test("recvDatagram does not commit the group cursor", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) });

	producer.insertDatagram(0, Timestamp.fromMillis(0), enc.encode("x"));
	expect((await track.recvDatagram())?.sequence).toBe(0);

	// Still uncommitted: the subscription can go sequence-ordered.
	const ordered = track.ordered();
	producer.writeGroup(new GroupProducer(1));
	expect((await ordered.nextGroup())?.sequence).toBe(1);
});

test("nextGroup returns undefined when track closes", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe().ordered();
	producer.close();
	expect(await track.nextGroup()).toBeUndefined();
});

// Close doesn't erase buffered frames: a group parked above the cap stays readable, so
// nextGroup must keep waiting for a cap raise rather than fake an end-of-track and lose
// the data (mirrors the Rust subscriber). Only a drained closed track reports finished.
test("a closed track still delivers a group parked above the cap once the cap is raised", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();

	for (let sequence = 0; sequence < 3; sequence++) {
		const group = new GroupProducer(sequence);
		group.writeFrame(textFrame(`frame-${sequence}`));
		group.close();
		producer.writeGroup(group);
	}

	track.setGroups({ end: { excluded: 2 } });
	expect((await track.nextGroup())?.sequence).toBe(0);
	expect((await track.nextGroup())?.sequence).toBe(1);

	// Group 2 parks above the cap; a clean close must not resolve it as finished.
	const parked = track.nextGroup();
	producer.close();
	const timeout = new Promise((resolve) => setTimeout(() => resolve("pending"), 10));
	expect(await Promise.race([parked, timeout])).toBe("pending");

	// Raising the cap after close releases the buffered group, frames intact.
	track.setGroups({ end: { excluded: 3 } });
	const released = await parked;
	expect(released?.sequence).toBe(2);
	expect(await released?.readString()).toBe("frame-2");

	// Drained and closed: now it's finished.
	expect(await track.nextGroup()).toBeUndefined();
});

// The final boundary counts datagrams: they share the group sequence namespace, and a
// Rust peer feeds SUBSCRIBE_END into finish_at, so a boundary that ignored a trailing
// datagram would finalize the track before it arrives.
test("end reports the head after the highest produced sequence, datagrams included", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();
	expect(track.end()).toBeUndefined();

	producer.writeGroup(new GroupProducer(0));
	producer.appendDatagram(Timestamp.fromMillis(1), enc.encode("x"));
	producer.close();
	expect(track.end()).toEqual({ group: 2, frame: 0 });
});

// 0 is the only encoding for "nothing produced"; an abort declares no boundary at all.
test("end is group 0 for an empty track and undefined after an abort", async () => {
	const empty = new TrackProducer("test");
	const emptyTrack = empty.subscribe();
	empty.close();
	expect(emptyTrack.end()).toEqual({ group: 0, frame: 0 });

	const aborted = new TrackProducer("test");
	const abortedTrack = aborted.subscribe();
	aborted.close(new Error("boom"));
	expect(abortedTrack.end()).toBeUndefined();
});

test("readFrame does not livelock when a sole group finishes before the next arrives", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe().ordered();

	// A group is appended then finished empty while the track stays open. A finished group's
	// readable() resolves immediately, so the reader must not busy-wait on it (which would starve the
	// macrotask queue and never observe the next group).
	const g0 = producer.appendGroup();
	g0.close();

	// The next group arrives via a macrotask; if the reader livelocks on microtasks it never runs.
	setTimeout(() => {
		const g1 = producer.appendGroup();
		g1.writeFrame(textFrame("hello"));
		g1.close();
		producer.close();
	}, 10);

	expect(await track.readString()).toBe("hello");
}, 2000);

// The IETF publisher resolves Largest Object from this, so it has to name a frame, not just
// a group: a filter is applied down to the object.
test("largest names the newest frame written", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe({ maxDelay: Milli(1_000) });

	// Nothing published yet, so the first read waits.
	const initial = subscriber.largest();
	expect(await Promise.race([initial.then(() => "ready"), Promise.resolve("pending")])).toBe("pending");

	const first = producer.appendGroup();
	first.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.now() });
	first.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.now() });
	expect(await initial).toEqual({ group: 0, frame: 1 });
	first.close();

	// A new group raises the largest at its head before any frame, so its first frame names
	// nothing newer.
	const second = producer.appendGroup();
	expect(await subscriber.largest()).toEqual({ group: 1, frame: 0 });

	second.writeFrame({ payload: enc.encode("c"), timestamp: Timestamp.now() });
	let advanced = false;
	const next = subscriber.largest().then((largest) => {
		advanced = true;
		return largest;
	});
	await settle();
	expect(advanced).toBe(false);
	second.writeFrame({ payload: enc.encode("d"), timestamp: Timestamp.now() });
	expect(await next).toEqual({ group: 1, frame: 1 });
	second.close();

	// Reading a group does not move the edge: it is what the track produced, not what is left.
	await subscriber.recvGroup();
	const ended = subscriber.largest();
	producer.close();
	expect(await ended).toBeUndefined();
});

// A subscriber that arrives after the frames were written reads mirrors. largest is the
// last frame successfully written, even after an overflow aborts the group.
test("largest ends at the declared end while the producer stays open", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();

	const group = producer.appendGroup();
	group.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.now() });
	expect(await subscriber.largest()).toEqual({ group: 0, frame: 0 });

	// The end is reached, but the open group can still raise the largest.
	producer.finishAt({ group: 1, frame: 0 });
	const ended = subscriber.largest();
	expect(await Promise.race([ended.then(() => "ready"), Promise.resolve("pending")])).toBe("pending");

	group.close();
	expect(await ended).toBeUndefined();
	producer.close();
});

// Every sequence below a reached end already raised the largest, so a late group below it
// changes nothing, and the ended stream stays ended.
test("a late group below the largest changes nothing", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();

	const framed = new GroupProducer(2);
	producer.writeGroup(framed);
	framed.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.now() });
	framed.close();
	const empty = new GroupProducer(5);
	producer.writeGroup(empty);
	empty.close();
	producer.finishAt({ group: 6, frame: 0 });

	expect(await subscriber.largest()).toEqual({ group: 5, frame: 0 });
	expect(await subscriber.largest()).toBeUndefined();

	// Late content below the end is still admitted; it just isn't the largest.
	const late = new GroupProducer(4);
	producer.writeGroup(late);
	late.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.now() });
	expect(await subscriber.largest()).toBeUndefined();
	expect(await producer.subscribe().largest()).toEqual({ group: 5, frame: 0 });
	producer.close();
});

// An aborted largest group can still be recreated, so the stream stays open past the end
// until a copy of it closes cleanly, and the recreated frames are reported before it ends.
test("a recreated largest group is reported before the end", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();
	// The recreated group carries the same content, timestamps included.
	const timestamp = Timestamp.now();
	const first = new GroupProducer(0);
	producer.writeGroup(first);
	first.writeFrame({ payload: enc.encode("a"), timestamp });
	first.close(new Error("cut"));
	producer.finishAt({ group: 1, frame: 0 });

	expect(await subscriber.largest()).toEqual({ group: 0, frame: 0 });
	let advanced = false;
	const next = subscriber.largest().then((largest) => {
		advanced = true;
		return largest;
	});
	await settle();
	expect(advanced).toBe(false);

	const again = new GroupProducer(0);
	producer.writeGroup(again);
	for (const payload of ["a", "b"]) again.writeFrame({ payload: enc.encode(payload), timestamp });
	expect(await next).toEqual({ group: 0, frame: 1 });
	again.close();
	expect(await subscriber.largest()).toBeUndefined();
	producer.close();
});

test("a datagram raises the largest", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();
	producer.appendGroup().close();
	expect(producer.appendDatagram(Timestamp.now(), enc.encode("d"))).toBe(1);
	expect(await subscriber.largest()).toEqual({ group: 1, frame: 0 });
	producer.close();
});

// A reader that starts at the largest group while it is still empty is at the live edge: it
// gets that group, which ends with nothing, then the next one.
test("a live-edge reader starts on an empty largest group", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const first = producer.appendGroup();
	first.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.now() });
	first.close();
	const empty = producer.appendGroup();

	const reader = producer.subscribe({ maxDelay: Milli(30_000) });
	const edge = await reader.largest();
	expect(edge).toEqual({ group: 1, frame: 0 });
	reader.setGroups({ start: { included: edge?.group ?? 0 } });

	empty.close();
	const next = producer.appendGroup();
	next.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.now() });

	const group = await reader.recvGroup();
	expect(group?.sequence).toBe(1);
	expect(await group?.readFrame()).toBeUndefined();
	const after = await reader.recvGroup();
	expect(after?.sequence).toBe(2);
	expect(await after?.readFrame()).toBeDefined();
	producer.close();
});

test("largest survives a mirror replay of an aborted group", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });

	const group = producer.appendGroup();
	for (let i = 0; i < MAX_GROUP_FRAMES; i++) {
		group.writeFrame({ payload: enc.encode(`${i}`), timestamp: Timestamp.now() });
	}
	expect(() => group.writeFrame({ payload: enc.encode("overflow"), timestamp: Timestamp.now() })).toThrow();

	// Subscribing now replays into a fresh sink; the last legal frame is still the edge.
	const late = producer.subscribe();
	expect(await late.largest()).toEqual({ group: 0, frame: MAX_GROUP_FRAMES - 1 });

	producer.close();
});

test("group ranges preserve progress and spell out inclusion", async () => {
	const producer = new TrackProducer("test");
	const track = producer
		.subscribe({ maxDelay: Milli(5000) })
		.withGroups({ start: { included: 1 }, end: { included: 1 } });
	for (let sequence = 0; sequence < 4; sequence++) producer.writeGroup(new GroupProducer(sequence));
	expect(track.tryRecvGroup()?.sequence).toBe(1);
	expect(track.tryRecvGroup()).toBeUndefined();
	track.setGroups({ end: { excluded: 3 } });
	expect(track.tryRecvGroup()?.sequence).toBe(2);
	expect(track.tryRecvGroup()).toBeUndefined();
	track.setGroups({ start: { included: 0 } });
	expect(track.tryRecvGroup()?.sequence).toBe(3);
	track.close();
});

test("serving replaceGroups can lower the floor that setGroups keeps", () => {
	const producer = new TrackProducer("test");
	const kept = producer.subscribe({ maxDelay: Milli(5000), groups: { start: { included: 10 } } });
	const lowered = producer.subscribe({ maxDelay: Milli(5000), groups: { start: { included: 10 } } });
	for (const sequence of [5, 6, 10]) producer.writeGroup(new GroupProducer(sequence));

	kept.setGroups({ start: { included: 5 } });
	expect(kept.tryRecvGroup()?.sequence).toBe(10);

	hooks.replaceGroups(lowered, { start: { included: 5 } });
	expect(lowered.tryRecvGroup()?.sequence).toBe(5);
	expect(lowered.tryRecvGroup()?.sequence).toBe(6);
	kept.close();
	lowered.close();
});

test("malformed group bounds do not partially advance the cursor", () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) });
	producer.writeGroup(new GroupProducer(0));
	expect(() => track.setGroups({ start: { included: 3 }, end: { excluded: -1 } })).toThrow();
	expect(track.tryRecvGroup()?.sequence).toBe(0);
	track.close();
});

test("finishAt refuses an end at or below a produced sequence", () => {
	const producer = new TrackProducer("test");
	producer.writeGroup(new GroupProducer(5));

	// Exclusive, so it must be above the highest produced sequence.
	expect(() => producer.finishAt({ group: 5, frame: 0 })).toThrow();
	expect(() => producer.finishAt({ group: 4, frame: 0 })).toThrow();
	producer.finishAt({ group: 6, frame: 0 });

	// A second end is refused, as is any group at or past the first.
	expect(() => producer.finishAt({ group: 7, frame: 0 })).toThrow();
	expect(() => producer.writeGroup(new GroupProducer(6))).toThrow();
	expect(() => producer.appendGroup()).toThrow();
	expect(() => producer.insertDatagram(6, Timestamp.now(), new Uint8Array(1))).toThrow();
	producer.close();
});

test("finishAt declares an end ahead of the live edge without ending the track", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(60_000) });
	producer.writeGroup(new GroupProducer(5));
	const first = await track.recvGroup();
	expect(first?.sequence).toBe(5);

	producer.finishAt({ group: 8, frame: 0 });
	expect(track.end()).toEqual({ group: 8, frame: 0 });
	expect(await track.finished()).toEqual({ group: 8, frame: 0 });

	// Not terminal: groups below the end still arrive, including a straggler.
	const late = new GroupProducer(7);
	late.close();
	producer.writeGroup(late);
	const straggler = new GroupProducer(6);
	straggler.close();
	producer.writeGroup(straggler);
	expect((await track.recvGroup())?.sequence).toBe(6);
	expect((await track.recvGroup())?.sequence).toBe(7);

	// A clean close keeps the declared end rather than the live edge.
	producer.close();
	expect(await track.recvGroup()).toBeUndefined();
	expect(track.end()).toEqual({ group: 8, frame: 0 });

	// A late subscriber sees the same end.
	expect(producer.subscribe().end()).toEqual({ group: 8, frame: 0 });
});

for (const ordered of [false, true]) {
	test(`a ${ordered ? "sequence" : "arrival"} reader ends at the declared boundary before closure`, async () => {
		const producer = new TrackProducer("test");
		const subscriber = producer.subscribe({ maxDelay: Milli(60_000) });
		const track = ordered ? subscriber.ordered() : subscriber;
		const receive = () => ("nextGroup" in track ? track.nextGroup() : track.recvGroup());
		try {
			producer.finishAt({ group: 3, frame: 0 });
			const waiting = receive();
			const group = new GroupProducer(2);
			group.writeFrame(textFrame("tail"));
			producer.writeGroup(group);
			const received = await waiting;
			expect(received?.sequence).toBe(2);
			expect(await received?.readString()).toBe("tail");

			// Groups 0 and 1 are missing and the last group is still open. Neither
			// prevents a reader ending, but closure and its abort remain separate.
			let ended = false;
			const end = receive().then((value) => {
				ended = true;
				expect(value).toBeUndefined();
			});
			await Promise.resolve();
			await Promise.resolve();
			expect(ended).toBe(true);
			await end;
			expect(track.closed.peek()).toBeUndefined();

			const aborted = new Error("cut off");
			producer.close(aborted);
			await expect(receive()).rejects.toThrow("cut off");
			await expect(received?.readString() ?? Promise.resolve()).rejects.toThrow("cut off");
		} finally {
			producer.close();
			track.close();
		}
	});
}

for (const channel of ["group", "datagram"]) {
	test(`a ${channel} reaching the end wakes both reader channels`, async () => {
		const producer = new TrackProducer("test");
		const track = producer.subscribe();
		try {
			producer.finishAt({ group: 3, frame: 0 });
			const groupRead = track.recvGroup();
			const datagramRead = track.recvDatagram();
			if (channel === "group") {
				const group = new GroupProducer(2);
				group.close();
				producer.writeGroup(group);
			} else {
				producer.insertDatagram(2, Timestamp.fromMillis(0), enc.encode("tail"));
			}
			expect((await groupRead)?.sequence).toBe(channel === "group" ? 2 : undefined);
			expect((await datagramRead)?.sequence).toBe(channel === "datagram" ? 2 : undefined);
			expect(await track.recvGroup()).toBeUndefined();
			expect(await track.recvDatagram()).toBeUndefined();
			expect(track.closed.peek()).toBeUndefined();
		} finally {
			producer.close();
			track.close();
		}
	});
}

test("finished rejects when the track aborts or closes without an end", async () => {
	const aborted = new TrackProducer("test");
	const pending = aborted.subscribe().finished();
	aborted.close(new Error("boom"));
	await expect(pending).rejects.toThrow("boom");

	const producer = new TrackProducer("test");
	const track = producer.subscribe();
	track.close();
	await expect(track.finished()).rejects.toThrow();

	// A clean close is an end, so it resolves.
	const clean = new TrackProducer("test");
	const reader = clean.subscribe();
	clean.appendGroup().close();
	clean.close();
	expect(await reader.finished()).toEqual({ group: 1, frame: 0 });
});

test("publisher max age is absent unless explicitly declared", () => {
	expect(infoDefaults().maxAge).toBeUndefined();
	expect(infoDefaults({ maxAge: Milli.zero }).maxAge).toBe(Milli.zero);
	expect(infoDefaults({ maxAge: Milli(30_000) }).maxAge).toBe(Milli(30_000));
});

// Rust applies the pool's idle expiry with `max_age: None` too: omitting the publisher
// limit only removes the media-time clamp, never the wall-clock bound on memory.
test("an omitted publisher limit still ages idle groups out", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("unlimited").accept({ timescale: Timescale.MILLI });
	try {
		const source = producer.appendGroup();
		source.writeFrame(textFrame("old"));
		source.close();
		clock.set(10_000 + CACHE_WINDOW_MS - 1_000);
		producer.appendGroup();
		const within = producer.subscribe({ maxDelay: Milli(100_000) });
		expect(await (await within.recvGroup())?.readString()).toBe("old");
		within.close();

		// A scan runs at most once per slice of the window, so step past the next one.
		clock.set(10_000 + CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8);
		const after = producer.subscribe({ maxDelay: Milli(100_000) });
		expect(after.tryRecvGroup()?.sequence).toBe(1);
		expect(after.tryRecvGroup()).toBeUndefined();
		after.close();
	} finally {
		producer.close();
		clock.restore();
	}
});

test("a live track does not pin an aborted newest group", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	try {
		const good = producer.appendGroup();
		good.writeFrame(textFrame("snapshot"));
		good.close();
		const reset = producer.appendGroup();
		reset.writeFrame(textFrame("partial"));
		reset.close(new Error("reset"));

		clock.set(10_000 + CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8);
		const late = producer.subscribe({ maxDelay: Milli(100_000) });
		expect(late.tryRecvGroup()).toBeUndefined();
		late.close();
	} finally {
		producer.close();
		clock.restore();
	}
});

test("closing a track keeps an idle newest group for subscribers to drain", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("catalog").accept({ timescale: Timescale.MILLI });
		const subscriber = producer.subscribe({ maxDelay: Milli(100_000) });
		const snapshot = producer.appendGroup();
		snapshot.writeFrame(textFrame("snapshot"));
		snapshot.close();

		// Idle well past the window, but protected while the track is live.
		clock.set(10_000 + CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8);
		producer.close();
		expect(await (await subscriber.recvGroup())?.readString()).toBe("snapshot");
		expect(await subscriber.recvGroup()).toBeUndefined();

		// The close restarted its window, which now runs out like any other.
		clock.set(10_000 + 2 * (CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8));
		const late = producer.subscribe({ maxDelay: Milli(100_000) });
		expect(late.tryRecvGroup()).toBeUndefined();
		late.close();
	} finally {
		clock.restore();
	}
});

test("a late subscriber to a closed unlimited track is released once the cache ages out", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("unlimited").accept({ timescale: Timescale.MILLI });
		const source = producer.appendGroup();
		source.writeFrame(textFrame("last"));
		source.close();
		producer.close();

		// Counts the map entries subscribing leaves behind, rather than the heap: any entry
		// keyed by the subscriber that outlives the cache window would pin it.
		const added: [Map<unknown, unknown>, unknown][] = [];
		const set = Map.prototype.set;
		const spy = spyOn(Map.prototype, "set").mockImplementation(function (this: Map<unknown, unknown>, key, value) {
			added.push([this, key]);
			return set.call(this, key, value);
		});
		let subscriber: ReturnType<TrackProducer["subscribe"]>;
		try {
			subscriber = producer.subscribe();
		} finally {
			spy.mockRestore();
		}

		const group = await subscriber.recvGroup();
		expect(await group?.readString()).toBe("last");
		expect(await subscriber.recvGroup()).toBeUndefined();
		subscriber.close();

		// A closed track protects nothing, so the next scan past the window reclaims it all.
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe().close();
		expect(added.filter(([map, key]) => map.has(key)).length).toBe(0);
	} finally {
		clock.restore();
	}
});

test("an abort keeps finished groups for a slow reader, then reports it", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(10_000) });
	const arrival = producer.subscribe({ maxDelay: Milli(10_000) });
	const ordered = producer.subscribe({ maxDelay: Milli(10_000) }).ordered();
	for (let i = 0; i < 2; i++) {
		const group = producer.appendGroup();
		group.writeFrame(textFrame(`g${i}`));
		group.close();
	}
	producer.appendGroup().writeFrame(textFrame("open"));
	const boom = new Error("boom");
	producer.close(boom);

	for (const next of [() => arrival.recvGroup(), () => ordered.nextGroup()]) {
		for (let i = 0; i < 2; i++) {
			const group = await next();
			expect(group?.sequence).toBe(i);
			expect(await group?.readString()).toBe(`g${i}`);
		}
		// The open group nobody will finish is not handed out.
		await expect(next()).rejects.toBe(boom);
	}
});

test("an abort after the declared end settles ends clean", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(10_000) });
	const arrival = producer.subscribe({ maxDelay: Milli(10_000) });
	const ordered = producer.subscribe({ maxDelay: Milli(10_000) }).ordered();
	for (let i = 0; i < 2; i++) {
		const group = producer.appendGroup();
		group.writeFrame(textFrame(`g${i}`));
		group.close();
	}
	producer.finishAt({ group: 2, frame: 0 });
	producer.close(new Error("boom"));

	for (const next of [() => arrival.recvGroup(), () => ordered.nextGroup()]) {
		expect((await next())?.sequence).toBe(0);
		expect((await next())?.sequence).toBe(1);
		expect(await next()).toBeUndefined();
	}
});

test("an abort after the declared end reached by a datagram ends clean", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	producer.appendDatagram(Timestamp.fromMillis(0), enc.encode("d"));
	producer.finishAt({ group: 1, frame: 0 });
	producer.close(new Error("boom"));
	expect(producer.closed.peek()).toBeNull();
});

test("an ended track's buffered groups age out for a stale subscriber", async () => {
	for (const abort of [undefined, new Error("boom")]) {
		const clock = manualTime(10_000);
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(30) });
		try {
			const stale = producer.subscribe({ maxDelay: Milli(30) });
			const group = producer.appendGroup();
			group.writeFrame(textFrame("x"));
			group.close();
			producer.close(abort);

			// Nothing writes after close, so only the scheduled cache cleanup runs.
			await clock.advance(10_000 + CACHE_WINDOW_MS + 100);
			if (abort) expect(() => stale.tryRecvGroup()).toThrow(abort);
			else expect(stale.tryRecvGroup()).toBeUndefined();
		} finally {
			producer.close();
			clock.restore();
		}
	}
});

test("an abort leaves a group already taken readable, then reports the abort", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(10_000) });
	const arrival = producer.subscribe({ maxDelay: Milli(10_000) });
	const ordered = producer.subscribe({ maxDelay: Milli(10_000) }).ordered();
	producer.appendGroup().writeFrame(textFrame("held"));
	const held = [await arrival.recvGroup(), await ordered.nextGroup()];
	const boom = new Error("boom");
	producer.close(boom);

	for (const group of held) {
		expect(group?.sequence).toBe(0);
		expect(await group?.readString()).toBe("held");
		await expect(group?.readFrame()).rejects.toBe(boom);
	}
});

/** Every group a subscriber can take right now, by sequence. */
function drain(track: { tryRecvGroup(): { sequence: number } | undefined }): number[] {
	const sequences: number[] = [];
	for (let group = track.tryRecvGroup(); group; group = track.tryRecvGroup()) sequences.push(group.sequence);
	return sequences;
}

/** An untimed track holding `count` single-frame groups. */
function untimedTrack(count: number): TrackProducer {
	const producer = new TrackProducer("test").accept({});
	for (let i = 0; i < count; i++) producer.writeFrame({ payload: enc.encode(`${i}`) });
	return producer;
}

test("a track is untimed unless it declares a timescale", () => {
	expect(infoDefaults().timescale).toBeUndefined();
	expect(infoDefaults({ timescale: Timescale.MICRO }).timescale).toBe(Timescale.MICRO);
});

test("a null timescale declares an untimed track, like an omitted one", () => {
	expect(infoDefaults({ timescale: null }).timescale).toBeUndefined();

	const producer = new TrackProducer("test").accept({ timescale: null });
	const group = producer.appendGroup();
	expect(() => group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1) })).toThrow(
		TimestampMismatch,
	);
	group.writeFrame({ payload: enc.encode("y") });
	expect(() => producer.appendDatagram(Timestamp.fromMillis(1), enc.encode("x"))).toThrow(TimestampMismatch);
	producer.appendDatagram(undefined, enc.encode("y"));
});

test("a frame whose timedness disagrees with its track is refused", async () => {
	const timed = new TrackProducer("timed").accept({ timescale: Timescale.MILLI });
	const group = timed.appendGroup();
	expect(() => group.writeFrame({ payload: enc.encode("x") })).toThrow(TimestampMismatch);
	group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1) });

	const untimed = new TrackProducer("untimed").accept({});
	const subscriber = untimed.subscribe();
	const other = untimed.appendGroup();
	expect(() => other.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1) })).toThrow(
		TimestampMismatch,
	);
	other.writeFrame({ payload: enc.encode("y") });
	other.close();

	// The untimed frame reaches the subscriber as it was written: no time filled in.
	const frame = await (await subscriber.recvGroup())?.readFrame();
	expect(frame && dec.decode(frame.payload)).toBe("y");
	expect(frame?.timestamp).toBeUndefined();
});

test("a group joining a track it disagrees with is refused", () => {
	const producer = new TrackProducer("test").accept({});
	const group = new GroupProducer(0);
	group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1) });
	expect(() => producer.writeGroup(group)).toThrow(TimestampMismatch);
});

test("an empty group bound to a timed track can't join an untimed one", () => {
	const timed = new TrackProducer("timed").accept({ timescale: Timescale.MILLI });
	const untimed = new TrackProducer("untimed").accept({});
	const group = timed.appendGroup();
	expect(() => untimed.writeGroup(group)).toThrow(TimestampMismatch);
	// Still held to the timed track it was bound to first.
	expect(() => group.writeFrame({ payload: enc.encode("x") })).toThrow(TimestampMismatch);
	group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1) });
});

test("a datagram whose timedness disagrees with its track is refused", async () => {
	const timed = new TrackProducer("timed").accept({ timescale: Timescale.MILLI });
	expect(() => timed.appendDatagram(undefined, enc.encode("x"))).toThrow(TimestampMismatch);

	const untimed = new TrackProducer("untimed").accept({});
	const subscriber = untimed.subscribe();
	expect(() => untimed.insertDatagram(0, Timestamp.fromMillis(1), enc.encode("x"))).toThrow(TimestampMismatch);
	untimed.appendDatagram(undefined, enc.encode("y"));
	const got = await subscriber.recvDatagram();
	expect(got?.timestamp).toBeUndefined();
});

// The wall clock judges an untimed track, so a new subscriber starts at the oldest cached
// group it doesn't find stale, the way a timed one does: a group is stale once its
// successor arrived a full budget ago.
test("an untimed subscriber starts at the oldest fresh group", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({});
	try {
		// Group N's successor arrives N + 1 seconds in.
		for (let i = 0; i < 5; i++) {
			producer.writeFrame({ payload: enc.encode(`${i}`) });
			clock.set(10_000 + (i + 1) * 1000);
		}

		// Four seconds after group 4 arrived, groups 0 to 2 were superseded 4, 3, and 2
		// seconds ago: past a 1.5s budget. Group 3 was superseded 1s ago.
		expect(drain(producer.subscribe({ maxDelay: Milli(1500) }))).toEqual([3, 4]);
		// No budget takes the newest group alone, which is never stale.
		const live = producer.subscribe();
		expect(drain(live)).toEqual([4]);
		expect(drain(producer.subscribe({ maxDelay: Milli(60_000) }))).toEqual([0, 1, 2, 3, 4]);

		producer.writeFrame({ payload: enc.encode("5") });
		expect(drain(live)).toEqual([5]);
	} finally {
		producer.close();
		clock.restore();
	}
});

test("an untimed subscriber without a start begins at the latest group under its end", () => {
	const producer = untimedTrack(5);
	// As the lite publisher serves a SUBSCRIBE with an end and no start.
	const subscriber = producer.subscribe({ groups: { end: { excluded: 3 } } });
	subscriber.setGroups({ end: { excluded: 3 } });
	expect(drain(subscriber)).toEqual([2]);
});

test("accept closes the track when something written before it disagrees", async () => {
	const timed = new TrackProducer("groups");
	timed.appendGroup().writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1) });
	expect(() => timed.accept({})).toThrow(TimestampMismatch);
	await expect(timed.info()).rejects.toBeInstanceOf(TimestampMismatch);

	const datagrams = new TrackProducer("datagrams");
	datagrams.subscribe();
	datagrams.appendDatagram(Timestamp.fromMillis(1), enc.encode("x"));
	expect(() => datagrams.accept({})).toThrow(TimestampMismatch);
});

test("a single-frame write that throws aborts its group", () => {
	const producer = new TrackProducer("test").accept({});
	const subscriber = producer.subscribe({ maxDelay: Milli(60_000) });
	// A timestamped frame is refused by an untimed track.
	expect(() => producer.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1) })).toThrow(
		TimestampMismatch,
	);
	producer.writeFrame({ payload: enc.encode("y") });

	// The failed group ends in that error instead of leaving a reader waiting on it.
	const failed = subscriber.tryRecvGroup();
	expect(failed?.sequence).toBe(0);
	expect(failed?.closed.peek()).toBeInstanceOf(TimestampMismatch);
	expect(drain(subscriber)).toEqual([1]);
});

// An explicit start is a floor, not a request: the budget still skips what it finds stale
// above it, as on a timed track.
test("an explicit start floors an untimed track", () => {
	const producer = untimedTrack(5);

	const resumed = producer.subscribe({ maxDelay: Milli(60_000), groups: { start: { included: 2 } } });
	expect(drain(resumed)).toEqual([2, 3, 4]);
	const live = producer.subscribe({ groups: { start: { included: 2 } } });
	expect(drain(live)).toEqual([4]);
});

test("explicit and producer-cut drops survive outside the group cache", async () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	hooks.dropTrackGroups(producer, 3, 4);
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 3, end: 4 });

	const group = new GroupProducer(4);
	producer.writeGroup(group);
	group.close(new Error("cut"));
	await settle();
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 4, end: 5 });

	reader.close();
	producer.close();
});

test("a drop below the window is reported once the window lowers to it", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000), groups: { start: { included: 5 } } });
	hooks.dropTrackGroups(producer, 2, 3);
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });

	hooks.replaceGroups(reader, { start: { included: 0 } });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 2, end: 3 });

	reader.close();
	producer.close();
});

test("a re-created cut group is served, not dropped", async () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	const cut = new GroupProducer(4);
	producer.writeGroup(cut);
	cut.close(new Error("cut"));
	await settle();

	const retry = new GroupProducer(4);
	producer.writeGroup(retry);
	await settle();
	const recv = hooks.tryRecvGroup(reader, true);
	expect(recv.kind === "group" ? recv.group.sequence : recv).toBe(4);

	reader.close();
	producer.close();
});

test("a wide drop is held and reported as one run", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	hooks.dropTrackGroups(producer, 2, 2 ** 40);
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 2, end: 2 ** 40 });

	// A wider drop reports only what this cursor has not reported yet.
	hooks.dropTrackGroups(producer, 1, 2 ** 40 + 2);
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 1, end: 2 });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 2 ** 40, end: 2 ** 40 + 2 });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });

	reader.close();
	producer.close();
});

test("a mid-group end survives eviction and a filtered late reader", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("video").accept({});
	try {
		producer.finishAt({ group: 5, frame: 5 });
		const group = new GroupProducer(5);
		producer.writeGroup(group);
		for (let i = 0; i < 5; i++) group.writeFrame({ payload: new Uint8Array([i]) });
		group.close();
		producer.close();
		await clock.advance(10_000 + CACHE_WINDOW_MS + 1);
		const reader = producer.subscribe().withGroups({ end: { excluded: 5 } });
		expect(await reader.largest()).toEqual({ group: 5, frame: 4 });
		expect(reader.end()).toEqual({ group: 5, frame: 5 });
		reader.close();
	} finally {
		producer.close();
		clock.restore();
	}
});

test("a wire end repeats or refines but never moves", () => {
	const producer = new TrackProducer("video").accept({});
	// A FETCH_OK names only the boundary; END_OF_TRACK may then name the frame before it.
	hooks.endTrack(producer, { group: 6, frame: 0 });
	hooks.endTrack(producer, { group: 5, frame: 5 });
	hooks.endTrack(producer, { group: 5, frame: 5 });
	expect(producer.subscribe().end()).toEqual({ group: 5, frame: 5 });
	for (const moved of [
		{ group: 5, frame: 4 },
		{ group: 6, frame: 0 },
		{ group: 5, frame: 0 },
	]) {
		expect(() => hooks.endTrack(producer, moved)).toThrow(ProtocolViolation);
	}
	producer.close();
});

// An end inside a group that already holds a frame at or past it would leave content the
// end says does not exist.
test("a mid-group end refuses to cut existing frames", () => {
	const producer = new TrackProducer("video").accept({});
	const group = new GroupProducer(5);
	producer.writeGroup(group);
	for (let i = 0; i < 6; i++) group.writeFrame({ payload: new Uint8Array([i]) });
	expect(() => producer.finishAt({ group: 5, frame: 5 })).toThrow(ProtocolViolation);
	expect(producer.subscribe().end()).toBeUndefined();
	producer.finishAt({ group: 5, frame: 6 });
	producer.close();
});

// A mid-group end caps its group, whether the group exists yet or not.
test.each([true, false])("a mid-group end caps later writes (declared first: %s)", (early) => {
	const producer = new TrackProducer("video").accept({});
	const end = { group: 5, frame: 2 };
	if (early) producer.finishAt(end);
	const group = new GroupProducer(5);
	group.writeFrame({ payload: new Uint8Array([0]) });
	producer.writeGroup(group);
	if (!early) producer.finishAt(end);
	group.writeFrame({ payload: new Uint8Array([1]) });
	expect(() => group.writeFrame({ payload: new Uint8Array([2]) })).toThrow("past the track's end");
	expect(group.consume().frameCount).toBe(2);
	expect(() => producer.writeGroup(new GroupProducer(6))).toThrow();
	// A group handed over already past the end is refused before it is cached.
	const overfull = new GroupProducer(5);
	for (let i = 0; i < 3; i++) overfull.writeFrame({ payload: new Uint8Array([i]) });
	group.close(new Error("replaced"));
	expect(() => producer.writeGroup(overfull)).toThrow(ProtocolViolation);
	producer.close();
});

// A closed group `sequence` written into `producer` whose one frame starts at `ms`, aborted
// as ingest does when that frame is refused.
function stampGroup(producer: TrackProducer, sequence: number, ms: number): GroupProducer {
	const group = new GroupProducer(sequence);
	producer.writeGroup(group);
	try {
		group.writeFrame({ payload: enc.encode(`${ms}`), timestamp: Timestamp.fromMillis(ms) });
	} catch (err) {
		group.close(err as Error);
		throw err;
	}
	group.close();
	return group;
}

function drainGroups(subscriber: ReturnType<TrackProducer["subscribe"]>): number[] {
	const sequences: number[] = [];
	for (let group = subscriber.tryRecvGroup(); group; group = subscriber.tryRecvGroup())
		sequences.push(group.sequence);
	return sequences;
}

test("a re-created group is handed out once", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(30_000) });
	const subscriber = producer.subscribe({ maxDelay: Milli(30_000) });
	stampGroup(producer, 0, 0);
	const aborted = new GroupProducer(1);
	producer.writeGroup(aborted);
	expect(drainGroups(subscriber)).toEqual([0, 1]);

	aborted.close(new Error("aborted"));
	stampGroup(producer, 1, 1);
	stampGroup(producer, 2, 2);
	expect(drainGroups(subscriber)).toEqual([2]);
});

// A gap is remembered until a group above it is past the budget, max delay or max age alike.
// Then every group below that one counts as handed out.
test("a gap is forgotten once a group above it is past the budget", () => {
	const time = manualTime(1_000);
	try {
		const readers = [
			{ info: { timescale: Timescale.MILLI, maxAge: Milli(30_000) }, maxDelay: Milli(10) },
			{ info: { timescale: Timescale.MILLI, maxAge: Milli(10) }, maxDelay: Milli(3_600_000) },
		];
		for (const { info, maxDelay } of readers) {
			const producer = new TrackProducer("test").accept(info);
			const subscriber = producer.subscribe({ maxDelay });
			// Group 3 never arrives.
			for (const sequence of [0, 1, 2, 4, 5, 6]) stampGroup(producer, sequence, sequence);
			expect(drainGroups(subscriber)).toEqual([0, 1, 2, 4, 5, 6]);
			// Group 4 reaches until group 5 presents at 5ms, so it is past a 10ms budget once the
			// edge presents at 15ms.
			for (let sequence = 7; sequence <= 14; sequence++) {
				stampGroup(producer, sequence, sequence);
				expect(drainGroups(subscriber)).toEqual([sequence]);
				expect(hooks.deliveredRuns(subscriber)).toBe(2);
			}
			stampGroup(producer, 15, 15);
			expect(drainGroups(subscriber)).toEqual([15]);
			expect(hooks.deliveredRuns(subscriber)).toBe(1);
		}
	} finally {
		time.restore();
	}
});

test("an endless track with gaps remembers only those within the budget", () => {
	const time = manualTime(1_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(30_000) });
		// Groups present 1ms apart, so the budget spans ten of them.
		const subscriber = producer.subscribe({ maxDelay: Milli(10) });
		let most = 0;
		for (let sequence = 0; sequence < 3000; sequence++) {
			// Every third group never arrives.
			if (sequence % 3 === 1) continue;
			stampGroup(producer, sequence, sequence);
			expect(drainGroups(subscriber)).toEqual([sequence]);
			most = Math.max(most, hooks.deliveredRuns(subscriber));
		}
		expect(most).toBe(5);

		// A late unseen group within the budget is still delivered.
		stampGroup(producer, 2995, 2995);
		expect(drainGroups(subscriber)).toEqual([2995]);
	} finally {
		time.restore();
	}
});

test("an appended rewind is refused and an equal start is not", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	producer.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.fromMillis(1000) });
	producer.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.fromMillis(1000) });
	expect(() => producer.writeFrame({ payload: enc.encode("c"), timestamp: Timestamp.fromMillis(999) })).toThrow(
		TimestampMismatch,
	);
	producer.writeFrame({ payload: enc.encode("d"), timestamp: Timestamp.fromMillis(1001) });
});

test("a rewind is refused whichever group lands first", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	stampGroup(producer, 5, 1000);
	expect(() => stampGroup(producer, 3, 2000)).toThrow(TimestampMismatch);
	stampGroup(producer, 3, 500);
	expect(() => stampGroup(producer, 4, 400)).toThrow(TimestampMismatch);
	expect(() => stampGroup(producer, 4, 1100)).toThrow(TimestampMismatch);
	stampGroup(producer, 4, 700);
});

// Subscribers may have seen an aborted group's start, so a replacement at its sequence leaves
// that start as the watermark, and no later group can start before it.
test("a replaced aborted group still bounds later ones", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const first = new GroupProducer(0);
	producer.writeGroup(first);
	first.writeFrame({ payload: enc.encode("1000"), timestamp: Timestamp.fromMillis(1000) });
	first.close(new Error("aborted"));

	const replacement = new GroupProducer(0);
	replacement.writeFrame({ payload: enc.encode("500"), timestamp: Timestamp.fromMillis(500) });
	producer.writeGroup(replacement);
	replacement.close();
	expect(() => stampGroup(producer, 1, 600)).toThrow(TimestampMismatch);
	stampGroup(producer, 1, 1000);
});

test("a rewind across large timescales is refused", () => {
	const m = Number.MAX_SAFE_INTEGER;
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	producer.writeFrame({ payload: enc.encode("a"), timestamp: new Timestamp(m - 1, Timescale(m)) });
	// (m - 2) / (m - 1) is earlier than (m - 1) / m, though both cross-products round alike.
	expect(() =>
		producer.writeFrame({ payload: enc.encode("b"), timestamp: new Timestamp(m - 2, Timescale(m - 1)) }),
	).toThrow(TimestampMismatch);
	// Exact for fractions too: 0.75s is later than 0.5s.
	const fractions = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	fractions.writeFrame({ payload: enc.encode("a"), timestamp: new Timestamp(0.5, Timescale.SECOND) });
	fractions.writeFrame({ payload: enc.encode("b"), timestamp: new Timestamp(750.5, Timescale.MILLI) });
	expect(() =>
		fractions.writeFrame({ payload: enc.encode("c"), timestamp: new Timestamp(1.5, Timescale(4)) }),
	).toThrow(TimestampMismatch);
});

test("a group stamped before it is written is checked on write", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	stampGroup(producer, 1, 6000);
	const group = new GroupProducer(2);
	group.writeFrame({ payload: enc.encode("early"), timestamp: Timestamp.fromMillis(5000) });
	expect(() => producer.writeGroup(group)).toThrow(TimestampMismatch);
});

test("an evicted group still bounds, and a late group below it is unverified", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(60_000) });
		for (const [sequence, ms] of [
			[0, 1000],
			[1, 2000],
			[2, 3000],
		]) {
			stampGroup(producer, sequence, ms);
		}

		// Idle past the cache window, so the next write evicts every group but the new edge.
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		const edge = producer.appendGroup();
		expect(() => edge.writeFrame({ payload: enc.encode("rewind"), timestamp: Timestamp.fromMillis(2500) })).toThrow(
			TimestampMismatch,
		);
		edge.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(3000) });

		// At or below the watermark the lower neighbor may be gone, so the watermark only caps
		// the group and it is accepted without being trusted to time anything.
		const track = producer.subscribe({ maxDelay: Milli(60_000) });
		expect(() => stampGroup(producer, 1, 4000)).toThrow(TimestampMismatch);
		stampGroup(producer, 1, 500);
		producer.close();

		const timestamps = new Map<number, Timestamp | undefined>();
		for (let group = await track.recvGroup(); group; group = await track.recvGroup()) {
			timestamps.set(group.sequence, hooks.groupTimestamp(group));
		}
		expect(timestamps.get(3)?.value).toBe(3000);
		expect(timestamps.has(1)).toBe(true);
		expect(timestamps.get(1)).toBeUndefined();
	} finally {
		clock.restore();
	}
});

test("an untimed track takes groups in any order", () => {
	const producer = new TrackProducer("test").accept({});
	for (const sequence of [5, 3, 4]) {
		const group = new GroupProducer(sequence);
		producer.writeGroup(group);
		group.writeFrame({ payload: enc.encode(`${sequence}`) });
		group.close();
	}
});

// A received track as a wire subscription leaves it: the feed starts at group 0 and its end is
// pending, so lower groups may still arrive behind a higher one.
function pendingTail(end: number, maxDelay = Milli(60_000)) {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ maxDelay });
	hooks.startTrack(producer, { start: 0 });
	hooks.endTrack(producer, { group: end, frame: 0 });
	const write = (sequence: number) => {
		const group = new GroupProducer(sequence);
		group.close();
		producer.writeGroup(group);
	};
	return { producer, reader, write };
}

/** Whether `read` settled once every pending microtask and timer callback ran. */
async function settled(read: Promise<unknown>): Promise<boolean> {
	let done = false;
	read.then(
		() => (done = true),
		() => (done = true),
	);
	await settle();
	return done;
}

test("a pending tail holds readers at a hole until it fills", async () => {
	const { producer, reader, write } = pendingTail(3);
	write(2);
	expect((await reader.recvGroup())?.sequence).toBe(2);
	let next = reader.recvGroup();
	expect(await settled(next)).toBe(false);

	write(0);
	expect((await next)?.sequence).toBe(0);
	next = reader.recvGroup();
	expect(await settled(next)).toBe(false);

	write(1);
	expect((await next)?.sequence).toBe(1);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(reader.closed.peek()).toBeUndefined();
	producer.close();
});

test("a pending tail ends readers at the boundary once the track closes", async () => {
	const { producer, reader, write } = pendingTail(2);
	write(1);
	expect((await reader.recvGroup())?.sequence).toBe(1);
	const next = reader.recvGroup();
	expect(await settled(next)).toBe(false);

	producer.close();
	expect(await next).toBeUndefined();
	expect(await reader.closed).toBeNull();
});

test("without a declared start, the first arrival stands in for it", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	hooks.endTrack(producer, { group: 3, frame: 0 });
	const group = new GroupProducer(2);
	group.close();
	producer.writeGroup(group);
	expect((await reader.recvGroup())?.sequence).toBe(2);
	expect(await reader.recvGroup()).toBeUndefined();
	producer.close();
});

// SUBSCRIBE_START answers the floor its SUBSCRIBE asked for, so it holds only while the demand
// keeps that floor: a lowered floor owes the groups it newly asks for.
test("a pending tail owes from SUBSCRIBE_START while the demand keeps its floor", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ maxDelay: Milli(60_000), groups: { start: { included: 1 } } });
	hooks.startTrack(producer, { start: 3, requested: 1 });
	hooks.endTrack(producer, { group: 4, frame: 0 });
	const group = new GroupProducer(3);
	group.close();
	producer.writeGroup(group);
	expect((await reader.recvGroup())?.sequence).toBe(3);
	expect(await reader.recvGroup()).toBeUndefined();

	const lower = producer.subscribe({ maxDelay: Milli(60_000), groups: { start: { included: 0 } } });
	expect((await lower.recvGroup())?.sequence).toBe(3);
	const next = lower.recvGroup();
	expect(await settled(next)).toBe(false);
	producer.close();
	expect(await next).toBeUndefined();
});

// The wire's end can land after the highest group, with lower groups still owed.
test("an end after the highest group holds the reader at the hole", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	hooks.startTrack(producer, { start: 0 });
	const high = new GroupProducer(2);
	high.close();
	producer.writeGroup(high);
	expect((await reader.recvGroup())?.sequence).toBe(2);
	const next = reader.recvGroup();

	hooks.endTrack(producer, { group: 3, frame: 0 });
	expect(await settled(next)).toBe(false);
	for (const sequence of [0, 1]) {
		const low = new GroupProducer(sequence);
		low.close();
		producer.writeGroup(low);
	}
	expect((await next)?.sequence).toBe(0);
	expect((await reader.recvGroup())?.sequence).toBe(1);
	expect(await reader.recvGroup()).toBeUndefined();
	producer.close();
});

// A group the tail still owed was cut off, so readers get the abort rather than a clean end
// without it.
test("an abort errors a pending tail short of a group", async () => {
	const { producer, reader, write } = pendingTail(2);
	write(1);
	expect((await reader.recvGroup())?.sequence).toBe(1);
	const next = reader.recvGroup();
	expect(await settled(next)).toBe(false);

	producer.close(new Error("dropped"));
	await expect(next).rejects.toThrow("dropped");
});

test("an abort after a full tail ends cleanly", async () => {
	const { producer, reader, write } = pendingTail(2);
	write(1);
	write(0);
	producer.close(new Error("dropped"));
	expect((await reader.recvGroup())?.sequence).toBe(0);
	expect((await reader.recvGroup())?.sequence).toBe(1);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(await reader.closed).toBeNull();
});

// A gap the grace already gave up on was lost before the abort, not cut off by it, so readers
// end cleanly. Within the grace, the same abort errors them.
for (const [waited, clean] of [
	[500, false],
	[1000, true],
] as const) {
	test(`an abort ${waited}ms after a hole opened ${clean ? "ends" : "errors"} a pending tail`, async () => {
		const clock = manualTime(0);
		try {
			// The subscription's max delay is the grace the hole is waited for.
			const { producer, reader, write } = pendingTail(3, Milli(1000));
			write(0);
			write(2);
			expect((await reader.recvGroup())?.sequence).toBe(0);
			expect((await reader.recvGroup())?.sequence).toBe(2);
			const next = reader.recvGroup();
			expect(await settled(next)).toBe(false);

			clock.set(waited);
			producer.close(new Error("dropped"));
			if (clean) expect(await next).toBeUndefined();
			else await expect(next).rejects.toThrow("dropped");
		} finally {
			clock.restore();
		}
	});
}

// Judged by what arrived, not by the cache, so an evicted group still counts.
test("an evicted group still fills a pending tail", async () => {
	const clock = manualTime(0);
	try {
		const { producer, reader, write } = pendingTail(3);
		for (const sequence of [0, 1]) {
			write(sequence);
			expect((await reader.recvGroup())?.sequence).toBe(sequence);
		}
		clock.set(CACHE_WINDOW_MS + 1000);
		write(2);
		// A late subscriber replays only what the cache still holds.
		expect(producer.subscribe({ maxDelay: Milli(60_000) }).tryRecvGroup()?.sequence).toBe(2);
		expect((await reader.recvGroup())?.sequence).toBe(2);
		expect(await reader.recvGroup()).toBeUndefined();
		producer.close();
	} finally {
		clock.restore();
	}
});

test("a datagram fills its sequence in a pending tail", async () => {
	const { producer, reader, write } = pendingTail(2);
	producer.insertDatagram(0, undefined, enc.encode("x"));
	write(1);
	expect((await reader.recvGroup())?.sequence).toBe(1);
	expect(await reader.recvGroup()).toBeUndefined();
	producer.close();
});

// SUBSCRIBE_DROP says the publisher gave the range up, so the tail owes nothing there: readers
// end at the boundary, and a later abort cut nothing off.
for (const abort of [false, true]) {
	test(`a dropped range ends a pending tail cleanly${abort ? " despite a later abort" : ""}`, async () => {
		const { producer, reader, write } = pendingTail(3);
		write(2);
		expect((await reader.recvGroup())?.sequence).toBe(2);
		const next = reader.recvGroup();
		expect(await settled(next)).toBe(false);

		hooks.dropTrackGroups(producer, 0, 2);
		if (abort) producer.close(new Error("dropped"));
		expect(await next).toBeUndefined();
		if (abort) expect(await reader.closed).toBeNull();
		else expect(reader.closed.peek()).toBeUndefined();
		producer.close();
	});
}
