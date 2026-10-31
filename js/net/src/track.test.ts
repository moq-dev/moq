import { expect, setSystemTime, spyOn, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { race } from "@moq/signals";
import { ProtocolViolation, StreamCode, StreamError, TimestampMismatch, TooFarBehind } from "./error.ts";
import { Decoder } from "./flate/decoder.ts";
import { Encoder } from "./flate/encoder.ts";
import { type Consumer as GroupConsumer, type Producer as GroupProducer, MAX_GROUP_FRAMES } from "./group.ts";
import { Version as IetfVersion } from "./ietf/version.ts";
import { hooks } from "./internal.ts";
import { textFrame } from "./mock.ts";
import { Timescale, Timestamp } from "./time.ts";
import { infoDefaults, Subscription, Producer as TrackProducer } from "./track.ts";
import { unwanted } from "./util/demand.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

// Matches the private idle cache window and Rust cache::DEFAULT_EXPIRY.
const CACHE_WINDOW_MS = 30_000;

test("a cached transport tail covers only positions it actually holds", async () => {
	const producer = new TrackProducer("test");
	const subscriber = producer.subscribe();
	const tail = hooks.receiveGroup(producer, { sequence: 0, start: 5 });
	tail.writeFrame({ payload: new Uint8Array([7]) });
	tail.finish();
	expect(hooks.cachedGroup(subscriber, 0)).toBeUndefined();
	const covered = hooks.cachedGroup(subscriber, 0, 5);
	expect((await covered?.readFrameSequence())?.sequence).toBe(5);
	covered?.close();
	// Distinct hostile starts cannot create one cache copy per request.
	for (let start = 6; start < 106; start++)
		expect(() => hooks.receiveGroup(producer, { sequence: 0, start })).toThrow("duplicate group");
	const retained = hooks.cachedGroup(subscriber, 0, 5);
	expect((await retained?.readFrameSequence())?.payload).toEqual(new Uint8Array([7]));
	retained?.close();
	subscriber.close();
	producer.finish();
	producer.close();
});

/** Let every pending microtask and timer callback run, so a parked read gets a turn. */
function settle(): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, 0));
}

// A manual clock reading `initial` milliseconds. `set` moves it to an absolute time and fires
// what came due; `advance` also drains what those timers schedule.
function manualTime(initial: number) {
	const clock = new Manual();
	const installed = clock.install();
	const to = (ms: number) => Nano(Nano.fromMilli(Milli(ms)) - clock.now());
	clock.jump(to(initial));
	return {
		set: (ms: number) => clock.jump(to(ms)),
		advance: (ms: number) => clock.advance(to(ms)),
		installed,
	};
}

test("wanted reflects subscriber demand and clears when the last one leaves", async () => {
	const producer = new TrackProducer("test");

	// No subscribers: no demand.
	expect(producer.demand().wanted.peek()).toBeUndefined();

	const a = producer.subscribe();
	const b = producer.subscribe().ordered();
	expect(producer.demand().wanted.peek()).toBeDefined();

	// Closing one of two keeps demand.
	a.close();
	expect(producer.demand().wanted.peek()).toBeDefined();

	// Closing the last subscriber drops demand. The consumer wire awaits this
	// to tear an idle upstream down.
	b.close();
	await unwanted(producer.demand());
	expect(producer.demand().wanted.peek()).toBeUndefined();
});

test("a producer never self-closes on zero demand: a publisher keeps serving new subscribers", async () => {
	const producer = new TrackProducer("video");

	// Demand comes and goes...
	const a = producer.subscribe();
	a.close();
	await unwanted(producer.demand());

	// ...but the producer stays open (only the wire acts on unwanted demand, not the producer),
	// so a publisher writing to a track nobody is watching is unaffected.
	expect(producer.closed.peek()).toBeUndefined();

	// A later subscriber still works and replays the cache.
	producer.writeFrame(textFrame("still here"));
	const b = producer.subscribe().ordered();
	expect(await b.readString()).toBe("still here");
	expect(producer.demand().wanted.peek()).toBeDefined();
});

test("unwanted resolves immediately when there was never any demand", async () => {
	const producer = new TrackProducer("video");
	// No subscriber was ever attached; the wait must not hang.
	await unwanted(producer.demand());
	expect(producer.demand().wanted.peek()).toBeUndefined();
});

test("wanted stays set across churn while at least one subscriber remains", async () => {
	const producer = new TrackProducer("video");

	const a = producer.subscribe();
	// Rapidly add and drop extra subscribers; `a` keeps demand asserted throughout.
	for (let i = 0; i < 20; i++) {
		const t = producer.subscribe();
		t.close();
		expect(producer.demand().wanted.peek()).toBeDefined();
	}

	a.close();
	await unwanted(producer.demand());
	expect(producer.demand().wanted.peek()).toBeUndefined();
});

// Append a group of a 3 byte first frame and a `delta` byte delta to an untimed track.
function refreshGroup(producer: TrackProducer, delta: number): void {
	const group = producer.appendGroup();
	group.writeFrame({ payload: enc.encode("key") });
	if (delta > 0) group.writeFrame({ payload: new Uint8Array(delta) });
	group.finish();
}

test("the first subscriber of a track with no group wants group 0", () => {
	const producer = new TrackProducer("test").accept({ refresh: 1 });
	expect(producer.demand().wanted.peek()).toBeUndefined();
	const subscriber = producer.subscribe();
	expect(producer.demand().wanted.peek()).toBe(0);
	subscriber.close();
});

// The cached group is served either way: refresh is a request, not a hold.
test("a join over the refresh ratio wants the group after the newest", async () => {
	for (const [delta, wanted] of [
		[0, 0],
		[3, 0],
		[4, 1],
	]) {
		const producer = new TrackProducer("test").accept({ refresh: 1 });
		refreshGroup(producer, delta);
		const subscriber = producer.subscribe();
		expect(producer.demand().wanted.peek()).toBe(wanted);
		expect((await subscriber.recvGroup())?.sequence).toBe(0);
		subscriber.close();
	}
});

test("a join wants past a datagram after the newest group", () => {
	const producer = new TrackProducer("test").accept({ refresh: 1 });
	refreshGroup(producer, 4);
	expect(producer.appendDatagram(undefined, enc.encode("dgram"))).toBe(1);
	const subscriber = producer.subscribe();
	expect(producer.demand().wanted.peek()).toBe(2);
	subscriber.close();
});

test("a datagram never overtakes a pending refresh", () => {
	const producer = new TrackProducer("test").accept({ refresh: 1 });
	refreshGroup(producer, 4);
	const subscriber = producer.subscribe();
	expect(producer.demand().wanted.peek()).toBe(1);

	expect(producer.appendDatagram(undefined, enc.encode("dgram"))).toBe(1);
	expect(producer.demand().wanted.peek()).toBe(2);

	// Group 2 answers it, and later datagrams ask for nothing.
	refreshGroup(producer, 0);
	producer.appendDatagram(undefined, enc.encode("dgram"));
	expect(producer.demand().wanted.peek()).toBe(2);
	subscriber.close();
});

test("joins coalesce until the wanted group lands", () => {
	const producer = new TrackProducer("test").accept({ refresh: 1 });
	refreshGroup(producer, 4);
	const subscribers = [producer.subscribe(), producer.subscribe()];
	expect(producer.demand().wanted.peek()).toBe(1);

	// Group 1 answers it, and a join under its ratio asks nothing more.
	refreshGroup(producer, 0);
	subscribers.push(producer.subscribe());
	expect(producer.demand().wanted.peek()).toBe(1);

	refreshGroup(producer, 4);
	subscribers.push(producer.subscribe());
	expect(producer.demand().wanted.peek()).toBe(3);
	for (const subscriber of subscribers) subscriber.close();
});

test("joins without refresh ask for no group", () => {
	const producer = new TrackProducer("test").accept({});
	refreshGroup(producer, 1000);
	const subscriber = producer.subscribe();
	expect(producer.demand().wanted.peek()).toBe(0);
	subscriber.close();
});

test("a fixed start is no join", () => {
	const producer = new TrackProducer("test").accept({ refresh: 1 });
	refreshGroup(producer, 4);
	const subscriber = producer.subscribe({ groups: { start: { included: 0 } } });
	expect(producer.demand().wanted.peek()).toBe(0);
	subscriber.close();
});

test("refresh must be a u32 ratio", () => {
	expect(() => infoDefaults({ refresh: -1 })).toThrow(RangeError);
	expect(() => infoDefaults({ refresh: 0.5 })).toThrow(RangeError);
	expect(() => infoDefaults({ refresh: 2 ** 32 })).toThrow(RangeError);
	expect(infoDefaults({ refresh: 0 }).refresh).toBe(0);
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
	const track = producer.subscribe().withRange({ start: { included: 5 }, end: { excluded: 7 } });
	for (const sequence of [4, 5, 7, 6]) {
		producer.insertDatagram(sequence, Timestamp.fromMillis(0), enc.encode("x"));
	}
	producer.finish();
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
		producer.finish();
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
	producer.finish();
	producer.close();
	for (let sequence = count - 64; sequence < count; sequence++) {
		expect((await slow.recvDatagram())?.sequence).toBe(sequence);
	}
	expect(await slow.recvDatagram()).toBeUndefined();
});

// A received compressed datagram inflates far past its wire size, so each reader inflates
// its own copy: a slow reader's queue holds only the slices, never what a fast one realized.
test("a datagram queue holds no plain copy another reader realized", async () => {
	const producer = new TrackProducer("test").accept({ compress: true });
	const slow = producer.subscribe();
	const fast = producer.subscribe();
	const plain = new Uint8Array(65535);
	const slice = new Encoder().frame(plain);
	expect(slice.length).toBeLessThan(128);
	const decode = spyOn(Decoder.prototype, "frame");
	try {
		const count = 64;
		for (let sequence = 0; sequence < count; sequence++) {
			hooks.writeDatagram(producer, { sequence, payload: slice }, plain.length);
		}
		const arrived = decode.mock.calls.length;
		const read = [];
		for (let sequence = 0; sequence < count; sequence++) {
			const datagram = await fast.recvDatagram();
			expect(datagram?.payload).toEqual(plain);
			read.push(datagram);
		}
		expect(decode.mock.calls.length).toBe(arrived + count);
		for (let sequence = 0; sequence < count; sequence++) {
			const datagram = await slow.recvDatagram();
			expect(datagram).not.toBe(read[sequence]);
			expect(datagram?.payload).toEqual(plain);
		}
		expect(decode.mock.calls.length).toBe(arrived + 2 * count);
	} finally {
		decode.mockRestore();
	}
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

	producer.createGroup(3);
	producer.createGroup(6);
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
	producer.finish();
	producer.close();
	expect(() => producer.insertDatagram(0, ts, enc.encode("x"))).toThrow("track is closed");
	expect(() => producer.appendDatagram(ts, enc.encode("x"))).toThrow("track is closed");
});

test("insertDatagram after abort is refused", () => {
	const producer = new TrackProducer("test");
	producer.abort(new Error("cancel"));
	expect(() => producer.insertDatagram(0, Timestamp.fromMillis(0), enc.encode("x"))).toThrow("track is closed");
});

test("appendDatagram rejects a payload over the QUIC datagram frame ceiling", () => {
	const producer = new TrackProducer("test");
	expect(() => producer.appendDatagram(Timestamp.fromMillis(0), new Uint8Array(65536))).toThrow();
	expect(() => producer.insertDatagram(0, Timestamp.fromMillis(0), new Uint8Array(65536))).toThrow();
});

test("subscriber options and updates are forwarded to the producer's aggregate", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli.zero, groups: { start: { included: 0 } } });

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
	producer.finish();
	producer.close();
});

// A typo from plain JS would otherwise be served as the default without a word.
test("an unknown subscriber order is refused", () => {
	const producer = new TrackProducer("test");
	expect(() => producer.subscribe({ order: "newest" } as unknown as Subscription)).toThrow(TypeError);
	expect(producer.subscription.peek()).toBeUndefined();
	producer.finish();
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

test("priority -128..=127 is accepted and anything outside is refused", () => {
	const producer = new TrackProducer("video");
	producer.accept({ priority: 127 });
	expect(producer.priority).toBe(127);
	expect(new TrackProducer("audio").subscribe({ priority: -128 }).subscription.peek()?.priority).toBe(-128);
	expect(() => producer.accept({ priority: 128 })).toThrow(RangeError);
	expect(() => producer.accept({ priority: -129 })).toThrow(RangeError);
	expect(() => producer.accept({ priority: 1.5 })).toThrow(RangeError);
	expect(() => producer.subscribe({ priority: 128 })).toThrow(RangeError);
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

	producer.createGroup(5);

	const first = await track.nextGroup();
	expect(first?.sequence).toBe(5);

	// Late arrivals with sequence <= last returned are skipped.
	producer.createGroup(3);
	producer.createGroup(4);
	producer.createGroup(7);

	const next = await track.nextGroup();
	expect(next?.sequence).toBe(7);
});

test("nextGroup returns buffered groups in sequence", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();

	producer.createGroup(3);
	producer.createGroup(5);

	expect((await track.nextGroup())?.sequence).toBe(3);
	expect((await track.nextGroup())?.sequence).toBe(5);
});

test.each([false, true])("a reordered tail keeps the group PTS drift verdict (B-frame: %s)", async (reordered) => {
	const clock = manualTime(0);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
		const subscriber = producer.subscribe({ maxDelay: Milli(80) });
		for (const ms of [0, 100]) {
			const group = producer.appendGroup();
			group.writeFrame({ payload: new Uint8Array(), timestamp: Timestamp.fromMillis(ms) });
			group.finish();
		}
		const edge = producer.appendGroup();
		for (const ms of [150, 200])
			edge.writeFrame({ payload: new Uint8Array(), timestamp: Timestamp.fromMillis(ms) });
		if (reordered) edge.writeFrame({ payload: new Uint8Array(), timestamp: Timestamp.fromMillis(160) });
		edge.finish();
		// Group 0 reaches 100ms and is 100ms behind the highest PTS; group 1 reaches 150ms.
		expect((await subscriber.recvGroup())?.sequence).toBe(1);
		subscriber.close();
		producer.finish();
		producer.close();
	} finally {
		clock.installed.close();
	}
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
	long.finish();
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
	long.finish();
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
	producer.createGroup(3);

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

// A frameless successor is not content, so the next started group bounds the group before
// it. That bound is sound: the frameless group's first frame is checked to start at or below
// the next group's, so the true reach can only be earlier.
test("a frameless successor leaves the next started group as the bound", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	// Any budget, so only media time judges: the wall clock barely moves here.
	const track = producer.subscribe({ maxDelay: Milli(500) });

	producer.writeFrame({ payload: enc.encode("0"), timestamp: Timestamp.fromMillis(0) });
	producer.appendGroup(); // seq 1 stalls before its first frame
	producer.writeFrame({ payload: enc.encode("2"), timestamp: Timestamp.fromMillis(10_000) });
	producer.writeFrame({ payload: enc.encode("3"), timestamp: Timestamp.fromMillis(11_000) });

	// Groups 0 and 1 both reach at most group 2's start, a second behind the edge: stale.
	expect(drain(track)).toEqual([2, 3]);
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
		const group = producer.createGroup(sequence);
		group.writeFrame({ payload: enc.encode(`${ms}`), timestamp: Timestamp.fromMillis(ms) });
		group.finish();
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

// Interleaving a group take with the frame helpers is still one cursor: a take that
// moves past the group the frame helpers were draining abandons it, so a later frame
// read never runs backwards.
for (const take of ["nextGroup", "tryNextGroup"] as const) {
	test(`${take} abandons the frame helpers' group when it passes it`, async () => {
		const producer = new TrackProducer("test");
		const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();

		const zero = producer.createGroup(0);
		zero.writeFrame(textFrame("0.0"));
		zero.writeFrame(textFrame("0.1"));
		zero.finish();

		const one = producer.createGroup(1);
		one.writeFrame(textFrame("1.0"));
		one.finish();

		const two = producer.createGroup(2);
		two.writeFrame(textFrame("2.0"));
		two.finish();

		// The frame path enters group 0; a direct take then claims group 1.
		expect((await track.readFrame())?.group).toBe(0);
		expect((await track[take]())?.sequence).toBe(1);

		// The frame path must not resume group 0 behind the cursor: it continues at 2.
		const next = await track.readFrame();
		expect(next?.group).toBe(2);
		expect(dec.decode(next?.payload)).toBe("2.0");
	});
}

// The frame helpers ride the same sequence cursor as nextGroup: a lower group arriving
// after a higher one was read is skipped, never fed to the caller backwards.
test("ordered frame reads skip a late lower-sequence group", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();

	const five = producer.createGroup(5);
	five.writeFrame(textFrame("five", Timestamp.fromMillis(5)));
	five.finish();
	expect(await track.readString()).toBe("five");

	const three = producer.createGroup(3);
	three.writeFrame(textFrame("three", Timestamp.fromMillis(3)));
	three.finish();

	const six = producer.createGroup(6);
	six.writeFrame(textFrame("six", Timestamp.fromMillis(6)));
	six.finish();

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
		clock.installed.close();
	}
});

test("the default subscription skips superseded groups and a bare options object sets no limit", async () => {
	expect(Subscription.default()).toEqual({ priority: 0, maxDelay: Milli.zero, order: "descending" });

	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const explicit = producer.subscribe(Subscription.default());
	const implicit = producer.subscribe();
	const bare = producer.subscribe({});
	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// No options means the default subscription, whose zero budget keeps only the newest group.
	expect((await explicit.recvGroup())?.sequence).toBe(2);
	expect((await implicit.recvGroup())?.sequence).toBe(2);
	// An omitted maxDelay leaves the publisher's retention as the only bound.
	for (const sequence of [0, 1, 2]) expect((await bare.recvGroup())?.sequence).toBe(sequence);
	expect(producer.subscription.peek()?.maxDelay).toBe(Milli(5000));
	producer.finish();
	producer.close();
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
		const group = producer.createGroup(sequence);
		group.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
		group.finish();
	}

	const track = producer.subscribe({ maxDelay: Milli(5000) });
	expect((await track.recvGroup())?.sequence).toBe(5);
	expect((await track.recvGroup())?.sequence).toBe(6);
	expect((await track.recvGroup())?.sequence).toBe(7);

	// Arriving below everything already delivered is not what makes content stale: the
	// budget is the only gate, and this straggler's timestamp is within it. A consumer
	// that needs sequence order reorders (or drops) it itself.
	const late = producer.createGroup(4);
	late.writeFrame({ payload: enc.encode("late"), timestamp: Timestamp.fromMillis(500) });
	late.finish();

	expect((await track.recvGroup())?.sequence).toBe(4);
	producer.finish();
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
	const named = producer.subscribe({ maxDelay: Milli.zero, groups: { start: { included: 1 } } });
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
		producer.finish();
		producer.close();

		expect((await track.recvGroup())?.sequence).toBe(1);
		expect(track.recvGroup()).resolves.toBeUndefined();
	} finally {
		clock.installed.close();
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
		open.finish();
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
		clock.installed.close();
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

test("a handed-out group reports its expiry to a writer racing it", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe();
	producer.writeFrame(textFrame("old"));

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect(await group.readString()).toBe("old");

	const write = new Promise<void>(() => {});
	const raced = race([write, group.expired]);
	producer.writeFrame(textFrame("new"));

	expect(await raced).toMatchObject({ code: StreamCode.Old, message: expect.stringContaining("max delay budget") });
	expect(group.expired.peek()).toMatchObject({ code: StreamCode.Old });
});

test("a frame still being written is judged against the budget", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });
	const track = producer.subscribe({ maxDelay: Milli(100) });
	const source = producer.appendGroup();
	source.writeFrame({ payload: enc.encode("old"), timestamp: Timestamp.fromMillis(0) });
	source.writeFrame({ payload: enc.encode("future"), timestamp: Timestamp.fromMillis(10_000) });

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect(await group.readFrame()).toBeDefined();
	const raced = race([new Promise<void>(() => {}), group.expired]);

	producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1_000) });
	// A group beyond the edge, so group 0's reach (1s) is provably behind it: a group is
	// bounded by where its successor begins, so the successor alone never convicts it.
	producer.writeFrame({ payload: enc.encode("later"), timestamp: Timestamp.fromMillis(2_000) });
	expect(await raced).toMatchObject({ code: StreamCode.Old });
});

// Mirrors the Rust `a_fully_read_group_still_reports_its_expiry`: a publisher still draining a
// group it read whole abandons the stream once the budget gives up on the group.
test("a fully read group still reports its expiry", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	try {
		const track = producer.subscribe({ maxDelay: Milli(1000) });
		const source = producer.appendGroup();
		source.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.fromMillis(0) });
		source.finish();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readFrame()).toBeDefined();
		expect(await group.readFrame()).toBeUndefined();
		expect(await group.closed).toBeNull();
		expect(group.expired.peek()).toBeUndefined();

		// Frozen media time, so only the wall clock since the successor arrived convicts it.
		producer.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.fromMillis(0) });
		const raced = race([new Promise<void>(() => {}), group.expired]);
		await settle();
		clock.set(10_999);
		await settle();
		expect(group.expired.peek()).toBeUndefined();

		clock.set(11_000);
		expect(await raced).toMatchObject({ code: StreamCode.Old });
		expect(group.expired.peek()).toMatchObject({ code: StreamCode.Old });
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
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
		producer.finish();
		producer.close();
		clock.installed.close();
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
		producer.finish();
		producer.close();
		clock.installed.close();
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
		producer.finish();
		producer.close();
		clock.installed.close();
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
			producer.finish();
			producer.close();
			clock.installed.close();
		}
	}
});

// Evicting a recorded successor nobody read leaves its mirror a reader-side gap, which is not
// a failure: the held group is still judged by that successor's arrival, not the next one's.
test("an evicted successor with unread frames still starts the wall clock", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	try {
		const budget = 2 * CACHE_WINDOW_MS;
		const track = producer.subscribe({ maxDelay: Milli(budget) });
		const write = () => {
			const group = producer.appendGroup();
			group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(0) });
			group.finish();
		};
		write();
		const group = track.tryRecvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("x");

		let stale = false;
		const guarded = race([new Promise<never>(() => {}), group.expired]).then((err) => {
			stale = true;
			return err;
		});
		write(); // the successor, left unread
		await settle();
		// A later live edge, so the successor ages out of the cache.
		await clock.advance(20_000);
		write();

		await clock.advance(10_000 + budget - 1);
		expect(stale).toBe(false);
		await clock.advance(10_000 + budget);
		expect(stale).toBe(true);
		expect(await guarded).toMatchObject({ code: StreamCode.Old });
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

// A held group's recorded successor that failed hands the wall clock on to the next live
// group, even when the write that brings that group in also evicts the failed one before the
// guard looks again, rather than convicting by the failed one's arrival.
test("a failed successor hands the wall clock on after the cache evicts it", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	try {
		const budget = 2 * CACHE_WINDOW_MS;
		const track = producer.subscribe({ maxDelay: Milli(budget) });
		const held = producer.appendGroup();
		held.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(0) });
		held.finish();
		const group = track.tryRecvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("x");

		let stale = false;
		const guarded = race([new Promise<never>(() => {}), group.expired]).then((err) => {
			stale = true;
			return err;
		});
		const failed = producer.appendGroup();
		await settle();

		// Past the cache window, so the next write evicts the failed successor at once.
		const replacement = 10_000 + CACHE_WINDOW_MS + 10_000;
		await clock.advance(replacement);
		failed.abort(new Error("upstream failed"));
		const live = producer.appendGroup();
		live.writeFrame({ payload: enc.encode("y"), timestamp: Timestamp.fromMillis(0) });
		live.finish();

		await clock.advance(10_000 + budget + 1);
		expect(stale).toBe(false);
		await clock.advance(replacement + budget - 1);
		expect(stale).toBe(false);
		await clock.advance(replacement + budget);
		expect(stale).toBe(true);
		expect(await guarded).toMatchObject({ code: StreamCode.Old });
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
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
			if (sequence % 2 === 0) group.finish();
		}
		producer.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(10) });
		expect(drain(producer.subscribe({ maxDelay: Milli(1000) }))).toHaveLength(RUN + 2);

		clock.set(11_000);
		expect(drain(track)).toEqual([RUN + 1]);
		expect(drain(producer.subscribe({ maxDelay: Milli(1000) }))).toEqual([RUN + 1]);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("a live track retains its newest closed group through idle expiry", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(100) });
	try {
		// A late older group must not replace the protected newest snapshot.
		for (const sequence of [2, 1]) {
			const group = producer.createGroup(sequence);
			group.writeFrame({ payload: enc.encode("snapshot"), timestamp: Timestamp.fromMillis(0) });
			group.finish();
		}
		await clock.advance(10_000 + CACHE_WINDOW_MS + 200);
		const replay = producer.subscribe({ maxDelay: Milli(100) });
		expect(replay.tryRecvGroup()?.sequence).toBe(2);
		expect(replay.tryRecvGroup()).toBeUndefined();
		replay.close();
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
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
		source.finish();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(new TextDecoder().decode((await group.readFrame())?.payload)).toBe("first");

		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.appendGroup();

		await expect(group.readFrame()).rejects.toThrow(TooFarBehind);
	} finally {
		clock.installed.close();
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
		source.finish();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(new TextDecoder().decode((await group.readFrame())?.payload)).toBe("first");

		producer.finish();
		producer.close();
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe({ maxDelay: Milli(100) });

		await expect(group.readFrame()).rejects.toBeInstanceOf(TooFarBehind);
	} finally {
		clock.installed.close();
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
		clock.installed.close();
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
		clock.installed.close();
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
		await clock.advance(10_000 + CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8);
		expect(await read).toBeInstanceOf(TooFarBehind);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
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
			producer.finish();
			producer.close();
			clock.installed.close();
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
		source.finish();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("only");
		expect(await group.readFrame()).toBeUndefined();

		producer.finish();
		producer.close();
		clock.set(10_000 + CACHE_WINDOW_MS + 200);
		producer.subscribe({ maxDelay: Milli(100) });

		expect(await group.readFrame()).toBeUndefined();
		expect(await group.closed).toBeNull();
	} finally {
		clock.installed.close();
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
		clock.installed.close();
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

	const edge = producer.createGroup(2);
	edge.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(2000) });
	edge.finish();
	expect((await track.recvGroup())?.sequence).toBe(2);

	const late = producer.createGroup(1);
	late.writeFrame({ payload: enc.encode("late"), timestamp: Timestamp.fromMillis(0) });
	late.finish();
	producer.finish();
	producer.close();

	expect(track.recvGroup()).resolves.toBeUndefined();
});

test("an aborted live-edge anchor does not make older content stale", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe();

	const edge = producer.createGroup(2);
	edge.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(2000) });
	edge.abort(new Error("aborted"));

	const older = producer.createGroup(1);
	older.writeFrame({ payload: enc.encode("older"), timestamp: Timestamp.fromMillis(0) });
	older.finish();

	expect((await track.recvGroup())?.sequence).toBe(1);
	track.close();
});

test.each(["arrival", "ordered"] as const)("subscription bounds stay reader-local for %s reads", async (order) => {
	const producer = new TrackProducer("test");
	const everything = producer.subscribe({ maxDelay: Milli(5000) });
	const subscriber = producer.subscribe({
		maxDelay: Milli(5000),
		groups: { start: { included: 1 }, end: { excluded: 3 } },
	});
	const ordered = order === "ordered" ? subscriber.ordered() : undefined;
	const read = () => (ordered ? ordered.nextGroup() : subscriber.recvGroup());
	try {
		for (let sequence = 0; sequence < 5; sequence++) producer.createGroup(sequence);
		expect((await everything.recvGroup())?.sequence).toBe(0);
		expect((await read())?.sequence).toBe(1);
		expect((await read())?.sequence).toBe(2);
		const pending = read();
		expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");
		subscriber.update({ maxDelay: Milli(5000), groups: { start: { included: 3 }, end: { excluded: 4 } } });
		expect((await pending)?.sequence).toBe(3);
		const capped = read();
		expect(await Promise.race([capped, Promise.resolve("pending")])).toBe("pending");
		subscriber.update({ maxDelay: Milli(5000), groups: { end: { included: 4 } } });
		// Arrival readers can now offer the unread group below the old floor; ordered ones cannot rewind.
		if (!ordered) expect((await capped)?.sequence).toBe(0);
		expect((await (ordered ? capped : read()))?.sequence).toBe(4);
		producer.finish();
		producer.close();
		expect(await read()).toBeUndefined();
	} finally {
		subscriber.close();
		everything.close();
		producer.finish();
		producer.close();
	}
});

test("subscription updates narrow the cursor before its first read", async () => {
	const producer = new TrackProducer("test");
	const everything = producer.subscribe({ maxDelay: Milli(5000) });
	const track = producer.subscribe({ maxDelay: Milli(5000) });
	try {
		for (let sequence = 0; sequence < 5; sequence++) producer.createGroup(sequence);
		track.update({ maxDelay: Milli(5000), groups: { start: { excluded: 2 }, end: { included: 3 } } });
		expect((await track.recvGroup())?.sequence).toBe(3);
		const pending = track.recvGroup();
		expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");
		track.close();
		await pending;
	} finally {
		track.close();
		everything.close();
		producer.finish();
		producer.close();
	}
});

test("a subscription cap chooses its own live edge", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const everything = producer.subscribe({ maxDelay: Milli(5000) });
	const track = producer.subscribe({ maxDelay: Milli.zero, groups: { end: { excluded: 2 } } });
	try {
		for (let sequence = 0; sequence < 5; sequence++) {
			producer.writeFrame({
				payload: enc.encode(`${sequence}`),
				timestamp: Timestamp.fromMillis(sequence * 1000),
			});
		}
		expect((await track.recvGroup())?.sequence).toBe(1);
		track.update({ maxDelay: Milli.zero, groups: { start: { included: 3 }, end: { excluded: 4 } } });
		expect((await track.recvGroup())?.sequence).toBe(3);
	} finally {
		track.close();
		everything.close();
		producer.finish();
		producer.close();
	}
});

test("subscription updates bound each reader's datagrams", async () => {
	const producer = new TrackProducer("test");
	const everything = producer.subscribe({ maxDelay: Milli(5000) });
	const track = producer.subscribe({ groups: { start: { included: 1 }, end: { excluded: 2 } } });
	try {
		for (let sequence = 0; sequence < 4; sequence++)
			producer.insertDatagram(sequence, undefined, new Uint8Array([sequence]));
		expect((await track.recvDatagram())?.sequence).toBe(1);
		track.update({ groups: { start: { included: 3 }, end: { excluded: 4 } } });
		for (let sequence = 2; sequence < 5; sequence++)
			producer.insertDatagram(sequence, undefined, new Uint8Array([sequence]));
		expect((await track.recvDatagram())?.sequence).toBe(3);
		const pending = track.recvDatagram();
		expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");
		track.close();
		await pending;
	} finally {
		track.close();
		everything.close();
		producer.finish();
		producer.close();
	}
});

test("withRange with an excluded end of 0 is the empty range and does not ride another subscriber's demand", async () => {
	const producer = new TrackProducer("test");
	const everything = producer.subscribe({ maxDelay: Milli(5000) });
	const empty = producer.subscribe({ maxDelay: Milli(5000), groups: { end: { excluded: 0 } } });
	empty.withRange({ end: { excluded: 0 } });

	for (let sequence = 0; sequence < 3; sequence++) producer.createGroup(sequence);

	expect((await everything.recvGroup())?.sequence).toBe(0);
	const pending = empty.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	empty.withRange({ end: { excluded: 2 } });
	expect((await pending)?.sequence).toBe(0);
	expect((await empty.recvGroup())?.sequence).toBe(1);

	empty.withRange({});
	expect((await empty.recvGroup())?.sequence).toBe(2);
});

test("withRange caps the live edge used by the latency budget", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe();
	track.withRange({ end: { excluded: 2 } });

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	expect((await track.recvGroup())?.sequence).toBe(1);
	track.withRange({});
	expect((await track.recvGroup())?.sequence).toBe(2);
});

// The frame helpers ride the group cursor, so its bounds apply to them too: a group
// above the cap parks (surviving a clean close) until the cap admits it.
test("withRange caps frame-level reads like the group cursor", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(5000) });
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();
	track.withRange({ end: { excluded: 1 } });

	producer.writeFrame({ payload: enc.encode("zero"), timestamp: Timestamp.fromMillis(0) });
	producer.writeFrame({ payload: enc.encode("one"), timestamp: Timestamp.fromMillis(1) });
	producer.finish();
	producer.close();

	expect((await track.readFrame())?.group).toBe(0);

	// Group 1 parks above the cap; a clean close must not resolve it as finished.
	const parked = track.readFrame();
	const timeout = new Promise((resolve) => setTimeout(() => resolve("pending"), 10));
	expect(await Promise.race([parked, timeout])).toBe("pending");

	// Raising the cap releases the parked group, frames intact.
	track.withRange({ end: { excluded: 2 } });
	expect((await parked)?.group).toBe(1);
	expect(await track.readFrame()).toBeUndefined();
});

test("local cursor bounds can skip, pause, and release buffered groups", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) }).ordered();

	for (let sequence = 0; sequence < 5; sequence++) producer.createGroup(sequence);

	track.withRange({ start: { included: 1 }, end: { excluded: 3 } });
	expect((await track.nextGroup())?.sequence).toBe(1);
	expect((await track.nextGroup())?.sequence).toBe(2);

	const pending = track.nextGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.withRange({ start: { included: 4 }, end: { excluded: 5 } });
	expect((await pending)?.sequence).toBe(4);
});

// A relay can ingest back-to-back groups micro-reordered (the upstream leg sends
// newest-first). The older group is cached and in demand, so serving must still
// deliver it; a sequence cursor would skip it permanently.
test("recvGroup serves a late arrival after a newer group", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) });

	producer.createGroup(2);
	expect((await track.recvGroup())?.sequence).toBe(2);

	// Group 1 lands after group 2 was already served.
	producer.createGroup(1);
	expect((await track.recvGroup())?.sequence).toBe(1);

	// Staleness is the latency window's job, not arrival order's: the track
	// still finishes normally afterward.
	producer.finish();
	producer.close();
	expect(await track.recvGroup()).toBeUndefined();
});

// recvGroup honors the withRange cap by parking, like nextGroup: beyond-cap groups are
// held, not dropped, and a raised cap re-offers them, even after a clean close.
test("withRange parks recvGroup beyond the cap and a raised cap re-offers", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) });

	for (let sequence = 0; sequence < 3; sequence++) producer.createGroup(sequence).finish();

	track.withRange({ end: { excluded: 2 } });
	expect((await track.recvGroup())?.sequence).toBe(0);
	expect((await track.recvGroup())?.sequence).toBe(1);

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	// A clean close keeps the parked group claimable: the cap may still rise.
	producer.finish();
	producer.close();
	expect(await Promise.race([pending, new Promise((resolve) => setTimeout(() => resolve("parked"), 10))])).toBe(
		"parked",
	);

	track.withRange({});
	expect((await pending)?.sequence).toBe(2);
	expect(await track.recvGroup()).toBeUndefined();
});

// A group beyond the cap must not block in-range groups that arrive behind it:
// a relay can ingest a burst micro-reordered (newest first).
test("recvGroup serves in-range groups that arrive behind a capped one", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) });

	track.withRange({ end: { excluded: 2 } });

	// Reordered burst: the beyond-cap group arrives first.
	producer.createGroup(2);
	producer.createGroup(0);
	producer.createGroup(1);

	expect((await track.recvGroup())?.sequence).toBe(0);
	expect((await track.recvGroup())?.sequence).toBe(1);

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.withRange({ end: { excluded: 3 } });
	expect((await pending)?.sequence).toBe(2);
});

// A raised floor drops parked groups it overtook instead of re-offering them once the cap
// rises.
test("withRange drops groups recvGroup parked at the cap", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	track.withRange({ end: { excluded: 1 } });
	producer.createGroup(1);
	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.withRange({ start: { included: 2 } });
	producer.createGroup(2);

	// The overtaken parked group is dropped, not re-offered.
	expect((await pending)?.sequence).toBe(2);
});

// A live duplicate would fan out to every subscriber twice (recvGroup has no sequence
// cursor to hide it), so createGroup rejects it like Rust's claim_sequence. An aborted
// incarnation is evicted so a fresh group can serve the sequence again.
test("createGroup rejects a duplicate live sequence", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	const first = producer.createGroup(7);
	first.writeFrame(textFrame("first"));
	first.finish();

	// A peer re-sending a live sequence is rejected, not fanned out again.
	expect(() => producer.createGroup(7)).toThrow("duplicate group");
	expect((await track.recvGroup())?.sequence).toBe(7);

	// An aborted incarnation is replaceable: the retry serves the sequence fresh.
	const aborted = producer.createGroup(8);
	aborted.abort(new Error("upstream reset"));

	const retry = producer.createGroup(8);
	retry.writeFrame(textFrame("retry"));
	retry.finish();

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

	track.withRange({ end: { excluded: 1 } });
	producer.createGroup(1);

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	producer.finish();
	producer.close();
	track.close();
	expect(await pending).toBeUndefined();
});

// Same, but with the producer still live: the first close() must also clear the
// buffer, or the parked group re-parks on wake and the read hangs forever.
test("closing the subscriber releases a recvGroup parked while the producer is live", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	track.withRange({ end: { excluded: 1 } });
	producer.createGroup(1);

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.close();
	expect(await pending).toBeUndefined();
});

test("the ordered and arrival cursors are independent", async () => {
	const producer = new TrackProducer("test");
	const ordered = producer.subscribe({ maxDelay: Milli(5000) }).ordered();
	const arrival = producer.subscribe({ maxDelay: Milli(5000) });

	producer.createGroup(5);
	expect((await ordered.nextGroup())?.sequence).toBe(5);

	// A late seq 3 is skipped by the ordered cursor, which has already passed it, but the
	// arrival cursor has both buffered and still delivers each exactly once.
	producer.createGroup(3);
	expect((await arrival.recvGroup())?.sequence).toBe(3);
	expect((await arrival.recvGroup())?.sequence).toBe(5);
});

// Both cursors draw from one buffer, so interleaving them produces a stream in neither
// order. The first group read commits the subscription; the other order is refused.
test("the first group read commits the cursor and refuses the other order", async () => {
	const producer = new TrackProducer("test");

	// An arrival read commits to arrival order; ordered() is refused afterwards.
	const arrival = producer.subscribe({ maxDelay: Milli(5000) });
	producer.createGroup(5);
	expect((await arrival.recvGroup())?.sequence).toBe(5);
	expect(() => arrival.ordered()).toThrow("arrival order");

	// The commitment refuses the other cursor without poisoning this one: a late
	// lower sequence still flows in arrival order.
	producer.createGroup(3);
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
	producer.createGroup(1);
	expect((await ordered.nextGroup())?.sequence).toBe(1);
});

test("nextGroup returns undefined when track closes", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe().ordered();
	producer.finish();
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
		const group = producer.createGroup(sequence);
		group.writeFrame(textFrame(`frame-${sequence}`));
		group.finish();
	}

	track.withRange({ end: { excluded: 2 } });
	expect((await track.nextGroup())?.sequence).toBe(0);
	expect((await track.nextGroup())?.sequence).toBe(1);

	// Group 2 parks above the cap; a clean close must not resolve it as finished.
	const parked = track.nextGroup();
	producer.finish();
	producer.close();
	const timeout = new Promise((resolve) => setTimeout(() => resolve("pending"), 10));
	expect(await Promise.race([parked, timeout])).toBe("pending");

	// Raising the cap after close releases the buffered group, frames intact.
	track.withRange({ end: { excluded: 3 } });
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
	expect(hooks.subscriberEnd(track)).toBeUndefined();

	producer.createGroup(0);
	producer.appendDatagram(Timestamp.fromMillis(1), enc.encode("x"));
	producer.finish();
	producer.close();
	expect(hooks.subscriberEnd(track)).toEqual({ group: 2, frame: 0 });
});

// 0 is the only encoding for "nothing produced"; an abort declares no boundary at all.
test("end is group 0 for an empty track and undefined after an abort", async () => {
	const empty = new TrackProducer("test");
	const emptyTrack = empty.subscribe();
	empty.finish();
	empty.close();
	expect(hooks.subscriberEnd(emptyTrack)).toEqual({ group: 0, frame: 0 });

	const aborted = new TrackProducer("test");
	const abortedTrack = aborted.subscribe();
	aborted.abort(new Error("boom"));
	expect(hooks.subscriberEnd(abortedTrack)).toBeUndefined();
});

test("readFrame does not livelock when a sole group finishes before the next arrives", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe().ordered();

	// A group is appended then finished empty while the track stays open. A finished group's
	// readable() resolves immediately, so the reader must not busy-wait on it (which would starve the
	// macrotask queue and never observe the next group).
	const g0 = producer.appendGroup();
	g0.finish();

	// The next group arrives via a macrotask; if the reader livelocks on microtasks it never runs.
	setTimeout(() => {
		const g1 = producer.appendGroup();
		g1.writeFrame(textFrame("hello"));
		g1.finish();
		producer.finish();
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
	expect(producer.largest()).toBeUndefined();
	const initial = subscriber.largest();
	expect(await Promise.race([initial.then(() => "ready"), Promise.resolve("pending")])).toBe("pending");

	const first = producer.appendGroup();
	first.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.now() });
	first.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.now() });
	expect(await initial).toEqual({ group: 0, frame: 1 });
	// The producer's getter holds no cursor, so every caller reads the same edge.
	expect(producer.largest()).toEqual({ group: 0, frame: 1 });
	expect(producer.largest()).toEqual({ group: 0, frame: 1 });
	first.finish();

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
	second.finish();

	// Reading a group does not move the edge: it is what the track produced, not what is left.
	await subscriber.recvGroup();
	const ended = subscriber.largest();
	producer.finish();
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

	group.finish();
	expect(await ended).toBeUndefined();
	producer.finish();
	producer.close();
});

// Every sequence below a reached end already raised the largest, so a late group below it
// changes nothing, and the ended stream stays ended.
test("a late group below the largest changes nothing", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();

	const framed = producer.createGroup(2);
	framed.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.now() });
	framed.finish();
	const empty = producer.createGroup(5);
	empty.finish();
	producer.finishAt({ group: 6, frame: 0 });

	expect(await subscriber.largest()).toEqual({ group: 5, frame: 0 });
	expect(await subscriber.largest()).toBeUndefined();

	// Late content below the end is still admitted; it just isn't the largest.
	const late = producer.createGroup(4);
	late.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.now() });
	expect(await subscriber.largest()).toBeUndefined();
	expect(await producer.subscribe().largest()).toEqual({ group: 5, frame: 0 });
	producer.finish();
	producer.close();
});

// An aborted largest group can still be recreated, so the stream stays open past the end
// until a copy of it closes cleanly, and the recreated frames are reported before it ends.
test("a recreated largest group is reported before the end", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();
	// The recreated group carries the same content, timestamps included.
	const timestamp = Timestamp.now();
	const first = producer.createGroup(0);
	first.writeFrame({ payload: enc.encode("a"), timestamp });
	first.abort(new Error("cut"));
	producer.finishAt({ group: 1, frame: 0 });

	expect(await subscriber.largest()).toEqual({ group: 0, frame: 0 });
	let advanced = false;
	const next = subscriber.largest().then((largest) => {
		advanced = true;
		return largest;
	});
	await settle();
	expect(advanced).toBe(false);

	const again = producer.createGroup(0);
	for (const payload of ["a", "b"]) again.writeFrame({ payload: enc.encode(payload), timestamp });
	expect(await next).toEqual({ group: 0, frame: 1 });
	again.finish();
	expect(await subscriber.largest()).toBeUndefined();
	producer.finish();
	producer.close();
});

test("a datagram raises the largest", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();
	producer.appendGroup().finish();
	expect(producer.appendDatagram(Timestamp.now(), enc.encode("d"))).toBe(1);
	expect(await subscriber.largest()).toEqual({ group: 1, frame: 0 });
	producer.finish();
	producer.close();
});

/** Write untimed group `sequence` with `frames` frames, left open. */
function framedGroup(producer: TrackProducer, sequence: number, frames: number): GroupProducer {
	const group = producer.createGroup(sequence);
	for (let i = 0; i < frames; i++) group.writeFrame({ payload: enc.encode("x") });
	return group;
}

/** Whether `promise` is still pending once queued work settles. */
async function isPending(promise: Promise<unknown>): Promise<boolean> {
	let settled = false;
	void promise.then(
		() => (settled = true),
		() => (settled = true),
	);
	await settle();
	return !settled;
}

// The largest reports only while it is within the bounds: a cap below it or a floor above it
// reports nothing, rather than a cached group standing in for it.
test("largest reports only within the bounds", async () => {
	const producer = new TrackProducer("test").accept({});
	for (let sequence = 0; sequence < 6; sequence++) framedGroup(producer, sequence, 2);

	const capped = producer.subscribe().withRange({ end: { excluded: 3 } });
	expect(await isPending(capped.largest())).toBe(true);

	// An inclusive cap reports the group it names.
	const ordered = producer
		.subscribe()
		.ordered()
		.withRange({ end: { included: 5 } });
	expect(await ordered.largest()).toEqual({ group: 5, frame: 1 });

	const floored = producer.subscribe().withRange({ start: { included: 8 } });
	const next = floored.largest();
	expect(await isPending(next)).toBe(true);
	framedGroup(producer, 8, 0);
	expect(await next).toEqual({ group: 8, frame: 0 });
	producer.finish();
	producer.close();
});

// Moving the bounds wakes a pending read, which reports the largest once it is within them;
// bounds that leave it where it was report nothing new.
test("largest follows the bounds as they move", async () => {
	const producer = new TrackProducer("test").accept({});
	for (let sequence = 0; sequence < 6; sequence++) framedGroup(producer, sequence, 1);

	const reader = producer
		.subscribe()
		.ordered()
		.withRange({ end: { excluded: 3 } });
	let next = reader.largest();
	expect(await isPending(next)).toBe(true);
	reader.withRange({});
	expect(await next).toEqual({ group: 5, frame: 0 });

	next = reader.largest();
	expect(await isPending(next)).toBe(true);
	reader.withRange({ end: { excluded: 2 } });
	expect(await isPending(next)).toBe(true);

	// Lifting it again leaves the last report standing.
	reader.withRange({});
	expect(await isPending(next)).toBe(true);
	framedGroup(producer, 6, 0);
	expect(await next).toEqual({ group: 6, frame: 0 });
	reader.withRange({ start: { included: 3 } });
	expect(await isPending(reader.largest())).toBe(true);
	producer.finish();
	producer.close();
});

// Once the stream ended, widening the bounds over a largest it never reported does not bring
// it back.
test("largest stays ended when the bounds widen", async () => {
	const producer = new TrackProducer("test").accept({});
	framedGroup(producer, 5, 1).finish();
	producer.finish();
	producer.close();

	const reader = producer.subscribe().withRange({ end: { excluded: 3 } });
	expect(await reader.largest()).toBeUndefined();
	reader.withRange({});
	expect(await reader.largest()).toBeUndefined();
});

// A reader that starts at the largest group while it is still empty is at the live edge: it
// gets that group, which ends with nothing, then the next one.
test("a live-edge reader starts on an empty largest group", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const first = producer.appendGroup();
	first.writeFrame({ payload: enc.encode("a"), timestamp: Timestamp.now() });
	first.finish();
	const empty = producer.appendGroup();

	const reader = producer.subscribe({ maxDelay: Milli(30_000) });
	const edge = await reader.largest();
	expect(edge).toEqual({ group: 1, frame: 0 });
	reader.withRange({ start: { included: edge?.group ?? 0 } });

	empty.finish();
	const next = producer.appendGroup();
	next.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.now() });

	const group = await reader.recvGroup();
	expect(group?.sequence).toBe(1);
	expect(await group?.readFrame()).toBeUndefined();
	const after = await reader.recvGroup();
	expect(after?.sequence).toBe(2);
	expect(await after?.readFrame()).toBeDefined();
	producer.finish();
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

	producer.finish();
	producer.close();
});

test("group ranges preserve progress and spell out inclusion", async () => {
	const producer = new TrackProducer("test");
	const track = producer
		.subscribe({ maxDelay: Milli(5000) })
		.withRange({ start: { included: 1 }, end: { included: 1 } });
	for (let sequence = 0; sequence < 4; sequence++) producer.createGroup(sequence);
	expect(track.tryRecvGroup()?.sequence).toBe(1);
	expect(track.tryRecvGroup()).toBeUndefined();
	track.withRange({ start: { included: 1 }, end: { excluded: 3 } });
	expect(track.tryRecvGroup()?.sequence).toBe(2);
	expect(track.tryRecvGroup()).toBeUndefined();
	track.withRange({ start: { included: 1 } });
	expect(track.tryRecvGroup()?.sequence).toBe(3);
	track.close();
});

// Mirrors the Rust `a_lowered_range_reoffers_the_unread_cached_groups`.
test("a lowered range re-offers the unread cached groups", () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000), groups: { start: { included: 10 } } });
	for (const sequence of [5, 6, 10]) producer.createGroup(sequence);
	expect(track.tryRecvGroup()?.sequence).toBe(10);
	expect(track.tryRecvGroup()).toBeUndefined();

	track.withRange({ start: { included: 6 } });
	expect(track.tryRecvGroup()?.sequence).toBe(6);
	track.withRange({ end: { included: 5 } });
	expect(track.tryRecvGroup()?.sequence).toBe(5);
	track.withRange({});
	expect(track.tryRecvGroup()).toBeUndefined();
	producer.createGroup(11);
	expect(track.tryRecvGroup()?.sequence).toBe(11);
	track.close();
});

test("a lowered range re-offers a cached cut with its error", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000), groups: { start: { included: 1 } } });
	const cut = producer.createGroup(0);
	const error = new StreamError(StreamCode(70), { message: "cut" });
	cut.abort(error);
	producer.createGroup(1);
	expect(track.tryRecvGroup()?.sequence).toBe(1);

	track.withRange({});
	const group = track.tryRecvGroup();
	expect(group?.sequence).toBe(0);
	await expect(group?.readString()).rejects.toBe(error);
	track.close();
	producer.finish();
	producer.close();
});

test("a lowered range still re-offers after the producer closes", () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000), groups: { start: { included: 1 } } });
	for (const sequence of [0, 1]) producer.createGroup(sequence).finish();
	expect(track.tryRecvGroup()?.sequence).toBe(1);
	producer.finish();
	producer.close();

	track.withRange({});
	expect(track.tryRecvGroup()?.sequence).toBe(0);
	expect(hooks.tryRecvGroup(track, false)).toEqual({ kind: "done" });
	track.close();
});

for (const abort of [undefined, new Error("gone")]) {
	test(`a lowered range offers nothing to a reader that ${abort ? "aborted" : "closed"}`, () => {
		const producer = new TrackProducer("test");
		const track = producer.subscribe({ maxDelay: Milli(5000), groups: { start: { included: 1 } } });
		for (const sequence of [0, 1]) producer.createGroup(sequence);
		expect(track.tryRecvGroup()?.sequence).toBe(1);
		track.close(abort);

		track.withRange({});
		expect(hooks.tryRecvGroup(track, false)).toEqual(abort ? { kind: "error", error: abort } : { kind: "done" });
		producer.finish();
		producer.close();
	});
}

test("malformed group bounds do not partially advance the cursor", () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(5000) });
	producer.createGroup(0);
	expect(() => track.withRange({ start: { included: 3 }, end: { excluded: -1 } })).toThrow();
	expect(track.tryRecvGroup()?.sequence).toBe(0);
	track.close();
});

test("finishAt refuses an end at or below a produced sequence", () => {
	const producer = new TrackProducer("test");
	producer.createGroup(5);

	// Exclusive, so it must be above the highest produced sequence.
	expect(() => producer.finishAt({ group: 5, frame: 0 })).toThrow();
	expect(() => producer.finishAt({ group: 4, frame: 0 })).toThrow();
	producer.finishAt({ group: 6, frame: 0 });

	// A second end is refused, as is any group at or past the first.
	expect(() => producer.finishAt({ group: 7, frame: 0 })).toThrow();
	expect(() => producer.createGroup(6)).toThrow();
	expect(() => producer.appendGroup()).toThrow();
	expect(() => producer.insertDatagram(6, Timestamp.now(), new Uint8Array(1))).toThrow();
	producer.finish();
	producer.close();
});

test("finishAt declares an end ahead of the live edge without ending the track", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxDelay: Milli(60_000) });
	producer.createGroup(5);
	const first = await track.recvGroup();
	expect(first?.sequence).toBe(5);

	producer.finishAt({ group: 8, frame: 0 });
	expect(hooks.subscriberEnd(track)).toEqual({ group: 8, frame: 0 });
	expect(await track.finished()).toEqual({ group: 8, frame: 0 });

	// Not terminal: groups below the end still arrive, including a straggler.
	const late = producer.createGroup(7);
	late.finish();
	const straggler = producer.createGroup(6);
	straggler.finish();
	expect((await track.recvGroup())?.sequence).toBe(6);
	expect((await track.recvGroup())?.sequence).toBe(7);

	// A clean close keeps the declared end rather than the live edge.
	producer.finish();
	producer.close();
	expect(await track.recvGroup()).toBeUndefined();
	expect(hooks.subscriberEnd(track)).toEqual({ group: 8, frame: 0 });

	// A late subscriber sees the same end.
	expect(hooks.subscriberEnd(producer.subscribe())).toEqual({ group: 8, frame: 0 });
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
			const group = producer.createGroup(2);
			group.writeFrame(textFrame("tail"));
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
			producer.abort(aborted);
			await expect(receive()).rejects.toThrow("cut off");
			await expect(received?.readString() ?? Promise.resolve()).rejects.toThrow("cut off");
		} finally {
			producer.finish();
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
				const group = producer.createGroup(2);
				group.finish();
			} else {
				producer.insertDatagram(2, Timestamp.fromMillis(0), enc.encode("tail"));
			}
			expect((await groupRead)?.sequence).toBe(channel === "group" ? 2 : undefined);
			expect((await datagramRead)?.sequence).toBe(channel === "datagram" ? 2 : undefined);
			expect(await track.recvGroup()).toBeUndefined();
			expect(await track.recvDatagram()).toBeUndefined();
			expect(track.closed.peek()).toBeUndefined();
		} finally {
			producer.finish();
			producer.close();
			track.close();
		}
	});
}

test("finished rejects when the track aborts or closes without an end", async () => {
	const aborted = new TrackProducer("test");
	const pending = aborted.subscribe().finished();
	aborted.abort(new Error("boom"));
	await expect(pending).rejects.toThrow("boom");

	const producer = new TrackProducer("test");
	const track = producer.subscribe();
	track.close();
	await expect(track.finished()).rejects.toThrow();

	// A clean close is an end, so it resolves.
	const clean = new TrackProducer("test");
	const reader = clean.subscribe();
	clean.appendGroup().finish();
	clean.finish();
	clean.close();
	expect(await reader.finished()).toEqual({ group: 1, frame: 0 });
});

// Rust applies the pool's idle expiry with `max_age: None` too: omitting the publisher
// limit only removes the media-time clamp, never the wall-clock bound on memory.
test("an omitted publisher limit still ages idle groups out", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("unlimited").accept({ timescale: Timescale.MILLI });
	try {
		const source = producer.appendGroup();
		source.writeFrame(textFrame("old"));
		source.finish();
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
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("a live track does not pin an aborted newest group", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	try {
		const good = producer.appendGroup();
		good.writeFrame(textFrame("snapshot"));
		good.finish();
		const reset = producer.appendGroup();
		reset.writeFrame(textFrame("partial"));
		reset.abort(new Error("reset"));

		clock.set(10_000 + CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8);
		const late = producer.subscribe({ maxDelay: Milli(100_000) });
		expect(late.tryRecvGroup()).toBeUndefined();
		late.close();
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("closing a track keeps an idle newest group for subscribers to drain", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("catalog").accept({ timescale: Timescale.MILLI });
		const subscriber = producer.subscribe({ maxDelay: Milli(100_000) });
		const snapshot = producer.appendGroup();
		snapshot.writeFrame(textFrame("snapshot"));
		snapshot.finish();

		// Idle well past the window, but protected while the track is live.
		clock.set(10_000 + CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8);
		producer.finish();
		producer.close();
		expect(await (await subscriber.recvGroup())?.readString()).toBe("snapshot");
		expect(await subscriber.recvGroup()).toBeUndefined();

		// The close restarted its window, which now runs out like any other.
		clock.set(10_000 + 2 * (CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8));
		const late = producer.subscribe({ maxDelay: Milli(100_000) });
		expect(late.tryRecvGroup()).toBeUndefined();
		late.close();
	} finally {
		clock.installed.close();
	}
});

test("a late subscriber to a closed unlimited track is released once the cache ages out", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("unlimited").accept({ timescale: Timescale.MILLI });
		const source = producer.appendGroup();
		source.writeFrame(textFrame("last"));
		source.finish();
		producer.finish();
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
		clock.installed.close();
	}
});

test("an abort keeps finished groups for a slow reader, then reports it", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(10_000) });
	const arrival = producer.subscribe({ maxDelay: Milli(10_000) });
	const ordered = producer.subscribe({ maxDelay: Milli(10_000) }).ordered();
	for (let i = 0; i < 2; i++) {
		const group = producer.appendGroup();
		group.writeFrame(textFrame(`g${i}`));
		group.finish();
	}
	producer.appendGroup().writeFrame(textFrame("open"));
	const boom = new Error("boom");
	producer.abort(boom);

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
		group.finish();
	}
	producer.finishAt({ group: 2, frame: 0 });
	producer.abort(new Error("boom"));

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
	producer.abort(new Error("boom"));
	expect(producer.closed.peek()).toBeNull();
});

test("a settled end survives a late group and an abort", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();
	const last = producer.createGroup(5);
	last.finish();
	producer.finishAt({ group: 6, frame: 0 });
	await settle();
	expect(producer.closed.peek()).toBeNull();

	producer.createGroup(4);
	producer.abort(new Error("boom"));
	expect(producer.closed.peek()).toBeNull();
	expect((await subscriber.recvGroup())?.sequence).toBe(5);
	// The abort cut nothing the end promised, so the subscriber ends cleanly.
	expect(await subscriber.recvGroup()).toBeUndefined();
});

// As in Rust, an evicted group no longer holds the end open.
test("closed settles once the cache evicts the last open group below the end", async () => {
	const clock = manualTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
		const open = producer.appendGroup();
		open.writeFrame(textFrame("open"));
		const last = producer.appendGroup();
		last.writeFrame(textFrame("last"));
		last.finish();
		producer.finish();
		await settle();
		expect(producer.closed.peek()).toBeUndefined();

		await clock.advance(10_000 + CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8);
		await settle();
		expect(open.closed.peek()).toBeInstanceOf(TooFarBehind);
		expect(producer.closed.peek()).toBeNull();
	} finally {
		clock.installed.close();
	}
});

test("finish is not terminal: closed settles once the groups below the end finish", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe();
	const group = producer.appendGroup();
	group.writeFrame(textFrame("late"));
	producer.finish();
	expect(producer.closed.peek()).toBeUndefined();

	// The group below the end is still writable after finish.
	group.writeFrame(textFrame("later"));
	group.finish();
	await settle();
	expect(producer.closed.peek()).toBeNull();
	// Settled, not released: the producer keeps serving its cache.
	expect(hooks.trackReleased(producer).peek()).toBeUndefined();

	const read = await subscriber.recvGroup();
	expect(read?.sequence).toBe(0);
	expect(await subscriber.recvGroup()).toBeUndefined();
	producer.close();
	expect(hooks.trackReleased(producer).peek()).toBeNull();
});

test("closed reports an abort before the end settles", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	producer.appendGroup().writeFrame(textFrame("open"));
	producer.finish();
	const abort = new Error("boom");
	producer.abort(abort);
	expect(producer.closed.peek()).toBe(abort);
});

test("close without finish aborts with a cancel", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	producer.writeFrame(textFrame("only"));
	producer.close();
	const closed = producer.closed.peek();
	expect(closed).toBeInstanceOf(StreamError);
	expect((closed as StreamError).code).toBe(StreamCode.Cancel);
});

test("an ended track's buffered groups age out for a stale subscriber", async () => {
	for (const abort of [undefined, new Error("boom")]) {
		const clock = manualTime(10_000);
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(30) });
		try {
			const stale = producer.subscribe({ maxDelay: Milli(30) });
			const group = producer.appendGroup();
			group.writeFrame(textFrame("x"));
			group.finish();
			if (abort) producer.abort(abort);
			else {
				producer.finish();
				producer.close();
			}

			// Nothing writes after close; the scheduled cleanup can lag by one scan slice.
			await clock.advance(10_000 + CACHE_WINDOW_MS + CACHE_WINDOW_MS / 8);
			if (abort) expect(() => stale.tryRecvGroup()).toThrow(abort);
			else expect(stale.tryRecvGroup()).toBeUndefined();
		} finally {
			producer.finish();
			producer.close();
			clock.installed.close();
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
	producer.abort(boom);

	for (const group of held) {
		expect(group?.sequence).toBe(0);
		expect(await group?.readString()).toBe("held");
		await expect(group?.readFrame()).rejects.toBe(boom);
	}
});

/** Every group a subscriber can take right now, by sequence. */
function drain(track: { tryRecvGroup(): GroupConsumer | undefined }): number[] {
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
	other.finish();

	// The untimed frame reaches the subscriber as it was written: no time filled in.
	const frame = await (await subscriber.recvGroup())?.readFrame();
	expect(frame && dec.decode(frame.payload)).toBe("y");
	expect(frame?.timestamp).toBeUndefined();
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

test("unknown track metadata keeps presentation-time expiry until accepted", () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("pending");
	try {
		for (let second = 0; second < 4; second++) {
			producer.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(second * 1000) });
		}
		// No wall time passes. Until metadata is accepted, buffered timestamps still
		// prove that the two oldest groups are outside this reader's media budget.
		expect(drain(producer.subscribe({ maxDelay: Milli(500) }))).toEqual([2, 3]);
		producer.accept({ timescale: Timescale.MILLI });
		expect(drain(producer.subscribe({ maxDelay: Milli(500) }))).toEqual([2, 3]);
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
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
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("an untimed subscriber without a start begins at the latest group under its end", () => {
	const producer = untimedTrack(5);
	// As the lite publisher serves a SUBSCRIBE with an end and no start.
	const subscriber = producer.subscribe({ maxDelay: Milli.zero, groups: { end: { excluded: 3 } } });
	subscriber.withRange({ end: { excluded: 3 } });
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
	const live = producer.subscribe({ maxDelay: Milli.zero, groups: { start: { included: 2 } } });
	expect(drain(live)).toEqual([4]);
});

// The error an upstream SUBSCRIBE_DROP gave groups up with.
const DROPPED = new StreamError(StreamCode.NotFound, { message: "dropped" });

test("explicit and producer-cut drops survive outside the group cache", async () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	hooks.dropTrackGroups(producer, 3, 4, DROPPED);
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 3, end: 4, error: DROPPED });

	const group = producer.createGroup(4);
	const cut = new Error("cut");
	group.abort(cut);
	await settle();
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 4, end: 5, error: cut });

	reader.close();
	producer.finish();
	producer.close();
});

test("a group ended by a cancel or cache policy is a hole, not a drop", async () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	const holes = [
		new StreamError(StreamCode.Cancel, { message: "cancel" }),
		new StreamError(StreamCode.Old, { message: "old" }),
		new StreamError(StreamCode.Evicted, { message: "evicted" }),
		new StreamError(StreamCode.DeliveryTimeout, { message: "late" }),
		new TooFarBehind(),
	];
	for (const [sequence, hole] of holes.entries()) {
		const group = producer.createGroup(sequence);
		group.abort(hole);
	}
	await settle();
	for (const sequence of holes.keys()) expect(hooks.givenUp(reader, sequence).error).toBeUndefined();

	reader.close();
	producer.finish();
	producer.close();
});

test("an upstream drop ends a group still arriving with its error", async () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	const arriving = producer.createGroup(3);
	arriving.writeFrame({ payload: enc.encode("3") });
	const finished = producer.createGroup(4);
	finished.writeFrame({ payload: enc.encode("4") });
	finished.finish();

	const group = await reader.recvGroup();
	expect(group?.sequence).toBe(3);
	expect(await group?.readString()).toBe("3");
	const cut = new StreamError(StreamCode(70), { message: "cut" });
	hooks.dropTrackGroups(producer, 3, 5, cut);
	// Settled by the drop itself, not a later reset; the finished group stays whole.
	await expect(group?.readString()).rejects.toBe(cut);
	const whole = await reader.recvGroup();
	expect(whole?.sequence).toBe(4);
	expect(await whole?.readString()).toBe("4");

	reader.close();
	producer.finish();
	producer.close();
});

test("a drop below the window is reported once the window lowers to it", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000), groups: { start: { included: 5 } } });
	hooks.dropTrackGroups(producer, 2, 3, DROPPED);
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });

	reader.withRange({ start: { included: 0 } });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 2, end: 3, error: DROPPED });

	reader.close();
	producer.finish();
	producer.close();
});

test("a re-created cut group is served, not dropped", async () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	const cut = producer.createGroup(4);
	cut.abort(new Error("cut"));
	await settle();

	producer.createGroup(4);
	await settle();
	const recv = hooks.tryRecvGroup(reader, true);
	expect(recv.kind === "group" ? recv.group.sequence : recv).toBe(4);

	reader.close();
	producer.finish();
	producer.close();
});

test("a reported drop still allows a late group, handed out once", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	hooks.dropTrackGroups(producer, 2, 4, DROPPED);
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 2, end: 4, error: DROPPED });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });
	producer.createGroup(3).finish();
	const recv = hooks.tryRecvGroup(reader, true);
	expect(recv.kind === "group" ? recv.group.sequence : recv).toBe(3);
	if (recv.kind === "group") recv.group.close();
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });
	reader.close();
	producer.finish();
	producer.close();
});

test("a wide drop is held and reported as one run", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	hooks.dropTrackGroups(producer, 2, 2 ** 40, DROPPED);
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 2, end: 2 ** 40, error: DROPPED });

	// A wider drop reports only what this cursor has not reported yet, with its own error.
	const wider = new StreamError(StreamCode.Old, { message: "old" });
	hooks.dropTrackGroups(producer, 1, 2 ** 40 + 2, wider);
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "drop", start: 1, end: 2, error: wider });
	expect(hooks.tryRecvGroup(reader, true)).toEqual({
		kind: "drop",
		start: 2 ** 40,
		end: 2 ** 40 + 2,
		error: wider,
	});
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });

	reader.close();
	producer.finish();
	producer.close();
});

test("adjacent drops with the same code merge into one run", () => {
	const producer = new TrackProducer("video").accept({});
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	// Each DROP decodes to its own error, so runs must merge by code, not identity.
	for (let sequence = 2; sequence < 5; sequence++) {
		hooks.dropTrackGroups(
			producer,
			sequence,
			sequence + 1,
			new StreamError(StreamCode.NotFound, { message: "dropped" }),
		);
	}
	const recv = hooks.tryRecvGroup(reader, true);
	expect(recv.kind === "drop" ? [recv.start, recv.end, recv.error] : recv).toEqual([2, 5, DROPPED]);
	expect(hooks.tryRecvGroup(reader, true)).toEqual({ kind: "idle" });

	reader.close();
	producer.finish();
	producer.close();
});

test("a mid-group end survives eviction and a filtered late reader", async () => {
	const clock = manualTime(10_000);
	const producer = new TrackProducer("video").accept({});
	try {
		producer.finishAt({ group: 5, frame: 5 });
		const group = producer.createGroup(5);
		for (let i = 0; i < 5; i++) group.writeFrame({ payload: new Uint8Array([i]) });
		group.finish();
		producer.finish();
		producer.close();
		await clock.advance(10_000 + CACHE_WINDOW_MS + 1);
		expect(await producer.subscribe().largest()).toEqual({ group: 5, frame: 4 });
		// The cap excludes the only group, so nothing is within it.
		const reader = producer.subscribe().withRange({ end: { excluded: 5 } });
		expect(await reader.largest()).toBeUndefined();
		expect(hooks.subscriberEnd(reader)).toEqual({ group: 5, frame: 5 });
		reader.close();
	} finally {
		producer.finish();
		producer.close();
		clock.installed.close();
	}
});

test("a wire end repeats or refines but never moves", () => {
	const producer = new TrackProducer("video").accept({});
	// A FETCH_OK names only the boundary; END_OF_TRACK may then name the frame before it.
	hooks.endTrack(producer, { group: 6, frame: 0 });
	hooks.endTrack(producer, { group: 5, frame: 5 });
	hooks.endTrack(producer, { group: 5, frame: 5 });
	expect(hooks.subscriberEnd(producer.subscribe())).toEqual({ group: 5, frame: 5 });
	for (const moved of [
		{ group: 5, frame: 4 },
		{ group: 6, frame: 0 },
		{ group: 5, frame: 0 },
	]) {
		expect(() => hooks.endTrack(producer, moved)).toThrow(ProtocolViolation);
	}
	producer.finish();
	producer.close();
});

// An end inside a group that already holds a frame at or past it would leave content the
// end says does not exist.
test("a mid-group end refuses to cut existing frames", () => {
	const producer = new TrackProducer("video").accept({});
	const group = producer.createGroup(5);
	for (let i = 0; i < 6; i++) group.writeFrame({ payload: new Uint8Array([i]) });
	expect(() => producer.finishAt({ group: 5, frame: 5 })).toThrow(ProtocolViolation);
	expect(hooks.subscriberEnd(producer.subscribe())).toBeUndefined();
	producer.finishAt({ group: 5, frame: 6 });
	producer.finish();
	producer.close();
});

// A mid-group end caps its group, whether the group exists yet or not.
test.each([true, false])("a mid-group end caps later writes (declared first: %s)", (early) => {
	const producer = new TrackProducer("video").accept({});
	const end = { group: 5, frame: 2 };
	if (early) producer.finishAt(end);
	const group = producer.createGroup(5);
	group.writeFrame({ payload: new Uint8Array([0]) });
	if (!early) producer.finishAt(end);
	group.writeFrame({ payload: new Uint8Array([1]) });
	expect(() => group.writeFrame({ payload: new Uint8Array([2]) })).toThrow("past the track's end");
	expect(group.consume().frameCount).toBe(2);
	expect(() => producer.createGroup(6)).toThrow();
	producer.finish();
	producer.close();
});

// A closed group `sequence` written into `producer` whose one frame starts at `ms`, aborted
// as ingest does when that frame is refused.
function stampGroup(producer: TrackProducer, sequence: number, ms: number): GroupProducer {
	const group = producer.createGroup(sequence);
	try {
		group.writeFrame({ payload: enc.encode(`${ms}`), timestamp: Timestamp.fromMillis(ms) });
	} catch (err) {
		group.abort(err as Error);
		throw err;
	}
	group.finish();
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
	const aborted = producer.createGroup(1);
	expect(drainGroups(subscriber)).toEqual([0, 1]);

	aborted.abort(new Error("aborted"));
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
		time.installed.close();
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
		time.installed.close();
	}
});

test("IETF provenance survives normalization on timed and untimed tracks", () => {
	for (const timescale of [undefined, Timescale.MILLI]) {
		const info = infoDefaults({ ietf: IetfVersion.DRAFT_22, timescale });
		expect(info.ietf).toBe(IetfVersion.DRAFT_22);
		expect(info.timescale).toBe(timescale);
	}
	const producer = new TrackProducer("ietf").accept({ timescale: Timescale.MILLI, ietf: IetfVersion.DRAFT_22 });
	try {
		stampGroup(producer, 0, 1000);
		stampGroup(producer, 1, 500);
		producer.writeFrame({ payload: enc.encode("arrival"), timestamp: Timestamp.fromMillis(250) });
	} finally {
		producer.finish();
		producer.close();
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
	const first = producer.createGroup(0);
	first.writeFrame({ payload: enc.encode("1000"), timestamp: Timestamp.fromMillis(1000) });
	first.abort(new Error("aborted"));

	const replacement = producer.createGroup(0);
	replacement.writeFrame({ payload: enc.encode("500"), timestamp: Timestamp.fromMillis(500) });
	replacement.finish();
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

test("a group's first frame is checked against the group below it", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	stampGroup(producer, 1, 6000);
	const group = producer.createGroup(2);
	expect(() => group.writeFrame({ payload: enc.encode("early"), timestamp: Timestamp.fromMillis(5000) })).toThrow(
		TimestampMismatch,
	);
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
		stampGroup(producer, 1, 500).finish();
		edge.finish();
		producer.finish();
		producer.close();

		const timestamps = new Map<number, Timestamp | undefined>();
		for (let group = await track.recvGroup(); group; group = await track.recvGroup()) {
			timestamps.set(group.sequence, hooks.groupTimestamp(group));
		}
		expect(timestamps.get(3)?.value).toBe(3000);
		expect(timestamps.has(1)).toBe(true);
		expect(timestamps.get(1)).toBeUndefined();
	} finally {
		clock.installed.close();
	}
});

test("an untimed track takes groups in any order", () => {
	const producer = new TrackProducer("test").accept({});
	for (const sequence of [5, 3, 4]) {
		const group = producer.createGroup(sequence);
		group.writeFrame({ payload: enc.encode(`${sequence}`) });
		group.finish();
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
		const group = producer.createGroup(sequence);
		group.finish();
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

// The end arrives after a higher group did, with lower ones still owed: it must not settle.
test("a received end behind a hole settles only once the hole fills", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	hooks.startTrack(producer, { start: 0 });
	const late = producer.createGroup(2);
	late.finish();
	hooks.endTrack(producer, { group: 3, frame: 0 });
	await settle();
	expect(producer.closed.peek()).toBeUndefined();

	for (const sequence of [0, 1]) {
		const group = producer.createGroup(sequence);
		group.finish();
	}
	await settle();
	expect(producer.closed.peek()).toBeNull();
	reader.close();
});

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
	producer.finish();
	producer.close();
});

test("a pending tail ends readers at the boundary once the track closes", async () => {
	const { producer, reader, write } = pendingTail(2);
	write(1);
	expect((await reader.recvGroup())?.sequence).toBe(1);
	const next = reader.recvGroup();
	expect(await settled(next)).toBe(false);

	producer.finish();
	producer.close();
	expect(await next).toBeUndefined();
	expect(await reader.closed).toBeNull();
});

test("without a declared start, the first arrival stands in for it", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	hooks.endTrack(producer, { group: 3, frame: 0 });
	const group = producer.createGroup(2);
	group.finish();
	expect((await reader.recvGroup())?.sequence).toBe(2);
	expect(await reader.recvGroup()).toBeUndefined();
	producer.finish();
	producer.close();
});

test("a source start follows the current requested floor after an update", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ groups: { start: { included: 5 } } });
	try {
		hooks.startTrack(producer, { start: 6, requested: 5 });
		expect(hooks.sourceStart(reader)).toBe(6);
		const lower = producer.subscribe({ groups: { start: { included: 2 } } });
		try {
			expect(hooks.sourceStart(lower)).toBe(2);
			expect(hooks.sourceStart(reader)).toBe(5);
		} finally {
			const restored = producer.subscription.changed();
			lower.close();
			await restored;
		}
		expect(hooks.sourceStart(reader)).toBe(6);
		const raised = producer.subscription.changed();
		reader.update({ groups: { start: { included: 9 } } });
		await raised;
		expect(hooks.sourceStart(reader)).toBe(9);
	} finally {
		reader.close();
		producer.finish();
		producer.close();
	}
});

// SUBSCRIBE_START answers the floor its SUBSCRIBE asked for, so it holds only while the demand
// keeps that floor: a lowered floor owes the groups it newly asks for.
test("a pending tail owes from SUBSCRIBE_START while the demand keeps its floor", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ maxDelay: Milli(60_000), groups: { start: { included: 1 } } });
	hooks.startTrack(producer, { start: 3, requested: 1 });
	hooks.endTrack(producer, { group: 4, frame: 0 });
	const group = producer.createGroup(3);
	group.finish();
	expect((await reader.recvGroup())?.sequence).toBe(3);
	expect(await reader.recvGroup()).toBeUndefined();

	const lower = producer.subscribe({ maxDelay: Milli(60_000), groups: { start: { included: 0 } } });
	expect((await lower.recvGroup())?.sequence).toBe(3);
	const next = lower.recvGroup();
	expect(await settled(next)).toBe(false);
	producer.finish();
	producer.close();
	expect(await next).toBeUndefined();
});

// The wire's end can land after the highest group, with lower groups still owed.
test("an end after the highest group holds the reader at the hole", async () => {
	const producer = new TrackProducer("test");
	const reader = producer.subscribe({ maxDelay: Milli(60_000) });
	hooks.startTrack(producer, { start: 0 });
	const high = producer.createGroup(2);
	high.finish();
	expect((await reader.recvGroup())?.sequence).toBe(2);
	const next = reader.recvGroup();

	hooks.endTrack(producer, { group: 3, frame: 0 });
	expect(await settled(next)).toBe(false);
	for (const sequence of [0, 1]) {
		const low = producer.createGroup(sequence);
		low.finish();
	}
	expect((await next)?.sequence).toBe(0);
	expect((await reader.recvGroup())?.sequence).toBe(1);
	expect(await reader.recvGroup()).toBeUndefined();
	producer.finish();
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

	producer.abort(new Error("dropped"));
	await expect(next).rejects.toThrow("dropped");
});

test("an abort after a full tail ends cleanly", async () => {
	const { producer, reader, write } = pendingTail(2);
	write(1);
	write(0);
	producer.abort(new Error("dropped"));
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
			producer.abort(new Error("dropped"));
			if (clean) expect(await next).toBeUndefined();
			else await expect(next).rejects.toThrow("dropped");
		} finally {
			clock.installed.close();
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
		producer.finish();
		producer.close();
	} finally {
		clock.installed.close();
	}
});

test("a datagram fills its sequence in a pending tail", async () => {
	const { producer, reader, write } = pendingTail(2);
	producer.insertDatagram(0, undefined, enc.encode("x"));
	write(1);
	expect((await reader.recvGroup())?.sequence).toBe(1);
	expect(await reader.recvGroup()).toBeUndefined();
	producer.finish();
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

		hooks.dropTrackGroups(producer, 0, 2, DROPPED);
		if (abort) producer.abort(new Error("dropped"));
		expect(await next).toBeUndefined();
		if (abort) expect(await reader.closed).toBeNull();
		else expect(reader.closed.peek()).toBeUndefined();
		producer.finish();
		producer.close();
	});
}

// A widened budget re-offers the cached groups the reader skipped as stale, and never one it
// already handed out.
test("a widened maxDelay re-offers what it skipped as stale", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe({ maxDelay: Milli(1500) });
	stampGroup(producer, 0, 0);
	expect(drainGroups(subscriber)).toEqual([0]);
	for (const [sequence, ms] of [
		[1, 1000],
		[2, 2000],
		[3, 3000],
		[4, 6000],
	]) {
		stampGroup(producer, sequence, ms);
	}
	// Groups 1 and 2 reach 2s and 3s, at least 1.5s behind the 6s edge.
	expect(drainGroups(subscriber)).toEqual([3, 4]);

	subscriber.update({ maxDelay: Milli(10_000) });
	expect(drainGroups(subscriber)).toEqual([1, 2]);
	expect(drainGroups(subscriber)).toEqual([]);
	producer.close();
});

// A read already parked when the budget widens wakes for the re-offer.
test("a widened maxDelay wakes a parked read", async () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe({ maxDelay: Milli(1500) });
	stampGroup(producer, 0, 0);
	expect(drainGroups(subscriber)).toEqual([0]);
	for (const [sequence, ms] of [
		[1, 1000],
		[2, 2000],
		[3, 6000],
	]) {
		stampGroup(producer, sequence, ms);
	}
	// Group 1 reaches 2s, 4s behind the 6s edge.
	expect(drainGroups(subscriber)).toEqual([2, 3]);

	const parked = subscriber.recvGroup();
	subscriber.update({ maxDelay: Milli(10_000) });
	expect((await parked)?.sequence).toBe(1);
	producer.close();
});

// A skip whose group the cache evicted can never be offered again, so it stops holding the
// delivered gaps above it.
test("an evicted stale skip lets the delivered gaps fold", () => {
	const time = manualTime(1_000);
	try {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI, maxAge: Milli(30_000) });
		const subscriber = producer.subscribe({ maxDelay: Milli(10) });
		stampGroup(producer, 0, 0);
		expect(drainGroups(subscriber)).toEqual([0]);
		// Group 1 reaches 5ms, past the 10ms budget behind the 30ms edge.
		stampGroup(producer, 1, 1);
		stampGroup(producer, 2, 5);
		stampGroup(producer, 3, 30);
		expect(drainGroups(subscriber)).toEqual([2, 3]);

		// Past the cache window, so the next write evicts group 1.
		time.set(1_000 + 2 * CACHE_WINDOW_MS);
		let most = 0;
		for (let sequence = 4; sequence < 300; sequence++) {
			// Every third group never arrives.
			if (sequence % 3 === 1) continue;
			stampGroup(producer, sequence, sequence + 30);
			expect(drainGroups(subscriber)).toEqual([sequence]);
			most = Math.max(most, hooks.deliveredRuns(subscriber));
		}
		expect(most).toBeLessThan(10);
	} finally {
		time.installed.close();
	}
});

// A re-offer never hands out a group below a floor raised in the same update.
test("a widened maxDelay re-offers nothing below a raised floor", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe({ maxDelay: Milli(1500) });
	stampGroup(producer, 0, 0);
	expect(drainGroups(subscriber)).toEqual([0]);
	for (const [sequence, ms] of [
		[1, 1000],
		[2, 2000],
		[3, 3000],
		[4, 6000],
	]) {
		stampGroup(producer, sequence, ms);
	}
	expect(drainGroups(subscriber)).toEqual([3, 4]);

	subscriber.update({ maxDelay: Milli(10_000), groups: { start: { included: 2 } } });
	expect(drainGroups(subscriber)).toEqual([2]);
	producer.close();
});

// A stale skip a drop-reading cursor has not reported yet is withdrawn when a widening re-offers
// its group, so the reader never gets both the group and its drop.
for (const reported of [false, true]) {
	test(`a widened maxDelay re-offers a stale group after its drop was ${reported ? "reported" : "pending"}`, () => {
		const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
		const subscriber = producer.subscribe({ maxDelay: Milli(1500) });
		expect(hooks.tryDrop(subscriber)).toBeUndefined();
		stampGroup(producer, 0, 0);
		expect(drainGroups(subscriber)).toEqual([0]);
		for (const [sequence, ms] of [
			[1, 1000],
			[2, 2000],
			[3, 6000],
		]) {
			stampGroup(producer, sequence, ms);
		}
		// Group 1 reaches 2s, 4s behind the 6s edge.
		expect(drainGroups(subscriber)).toEqual([2, 3]);

		if (reported) {
			expect(hooks.tryDrop(subscriber)).toMatchObject({ start: 1, end: 2 });
			expect(hooks.tryDrop(subscriber)).toBeUndefined();
		}
		subscriber.update({ maxDelay: Milli(10_000) });
		expect(drainGroups(subscriber)).toEqual([1]);
		expect(hooks.tryDrop(subscriber)).toBeUndefined();
		producer.close();
	});
}

// A widening that covers only the newer of two skipped groups hands it out without folding the
// older one into the delivered floor: a further widening still re-offers it.
test("a partial maxDelay widening keeps the older skip owed", () => {
	const producer = new TrackProducer("test").accept({ timescale: Timescale.MILLI });
	const subscriber = producer.subscribe({ maxDelay: Milli(1500) });
	stampGroup(producer, 0, 0);
	expect(drainGroups(subscriber)).toEqual([0]);
	for (const [sequence, ms] of [
		[1, 1000],
		[2, 2000],
		[3, 4000],
	]) {
		stampGroup(producer, sequence, ms);
	}
	expect(drainGroups(subscriber)).toEqual([2, 3]);
	for (const [sequence, ms] of [
		[4, 5000],
		[5, 6000],
		[6, 9000],
	]) {
		stampGroup(producer, sequence, ms);
	}
	expect(drainGroups(subscriber)).toEqual([5, 6]);

	// Group 4 reaches 6s, 3s behind the 9s edge; group 1 reaches 2s, 7s behind.
	subscriber.update({ maxDelay: Milli(4000) });
	expect(drainGroups(subscriber)).toEqual([4]);
	subscriber.update({ maxDelay: Milli(10_000) });
	expect(drainGroups(subscriber)).toEqual([1]);
	producer.close();
});
