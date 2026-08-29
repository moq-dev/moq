import { expect, setSystemTime, test } from "bun:test";
import { Producer as GroupProducer, Lagged } from "./group.ts";
import { hooks } from "./internal.ts";
import { Timestamp } from "./time.ts";
import { Producer as TrackProducer } from "./track.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

/** Let every pending microtask and timer callback run, so a parked read gets a turn. */
function settle(): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, 0));
}

function mockMonotonicTime(initial: number) {
	let now = initial;
	const real = performance.now.bind(performance);
	performance.now = () => now;
	return {
		set: (value: number) => {
			now = value;
		},
		restore: () => {
			performance.now = real;
		},
	};
}

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
		maxAge: 0,
		startGroup: 0,
		endGroup: undefined,
	});

	// The wire layer watches the producer's signal to emit SUBSCRIBE_UPDATE.
	const next = producer.subscription.changed();
	track.update({ priority: 7, ordered: true, maxAge: 250, startGroup: 2, endGroup: 9 });
	expect(await next).toEqual({ priority: 7, ordered: true, maxAge: 250, startGroup: 2, endGroup: 9 });
});

test("multiple subscriber options aggregate like Rust", async () => {
	const producer = new TrackProducer("test");
	const bounded = producer.subscribe({ priority: 2, ordered: true, maxAge: 100, startGroup: 10, endGroup: 20 });
	const live = producer.subscribe({ priority: 7, ordered: false, maxAge: 250, startGroup: 5 });

	expect(producer.subscription.peek()).toEqual({
		priority: 7,
		ordered: false,
		maxAge: 250,
		startGroup: 5,
		endGroup: undefined,
	});

	const narrowed = producer.subscription.changed();
	live.close();
	expect(await narrowed).toEqual({
		priority: 2,
		ordered: true,
		maxAge: 100,
		startGroup: 10,
		endGroup: 20,
	});

	const none = producer.subscription.changed();
	bounded.close();
	expect(await none).toBeUndefined();
});

test("the producer aggregate is clamped without changing subscriber options", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxAge: 10_000 });
	const clamped = producer.subscription.changed();

	producer.accept({ maxAge: 2_000 });

	expect((await clamped)?.maxAge).toBe(2_000);
	expect(track.subscription.peek()?.maxAge).toBe(10_000);
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
	const track = producer.subscribe({ maxAge: 5000 });

	producer.writeGroup(new GroupProducer(3));
	producer.writeGroup(new GroupProducer(5));

	expect((await track.nextGroup())?.sequence).toBe(3);
	expect((await track.nextGroup())?.sequence).toBe(5);
});

test("latency budget skips a buffered timeline in every group read", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 5000 });
	const arrival = producer.subscribe();
	const ordered = producer.subscribe();
	const frames = producer.subscribe();

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	expect((await arrival.recvGroup())?.sequence).toBe(2);
	expect((await ordered.nextGroup())?.sequence).toBe(2);
	expect((await frames.readFrameSequence())?.group).toBe(2);
});

test("zero latency takes the latest group when ages are equal", async () => {
	const clock = mockMonotonicTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ maxAge: 5000 });
		const track = producer.subscribe();
		producer.writeString("old");
		producer.writeString("new");

		expect((await track.recvGroup())?.sequence).toBe(1);
	} finally {
		clock.restore();
	}
});

test("latency budget admits groups within its presentation-time window", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 5000 });
	const track = producer.subscribe({ maxAge: 1500 });

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	expect((await track.recvGroup())?.sequence).toBe(1);
	expect((await track.recvGroup())?.sequence).toBe(2);
});

test("a late lower group within the budget is delivered", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 30_000 });
	for (const [sequence, timestamp] of [
		[5, 0],
		[6, 1000],
		[7, 2000],
	]) {
		const group = new GroupProducer(sequence);
		producer.writeGroup(group);
		group.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
		group.close();
	}

	const track = producer.subscribe({ maxAge: 5000 });
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
	const producer = new TrackProducer("test").accept({ maxAge: 30_000 });
	for (const timestamp of [0, 1000, 2000, 3000, 4000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// The budget is the only thing that asks for data; a named start only bounds how far
	// back it may reach.
	const floored = producer.subscribe({ startGroup: 3, maxAge: 60_000 });
	expect((await floored.recvGroup())?.sequence).toBe(3);
	expect((await floored.recvGroup())?.sequence).toBe(4);

	// Naming group 1 at real time still delivers the live edge alone: the zero budget
	// calls everything older stale.
	const named = producer.subscribe({ startGroup: 1 });
	expect((await named.recvGroup())?.sequence).toBe(4);
});

test("the budget is clamped to the publisher's window", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 1500 });
	const track = producer.subscribe({ maxAge: 60_000 });

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	// Asking to tolerate a minute cannot reach back further than the publisher keeps a group
	// live, the same clamp delivery applies.
	expect((await track.recvGroup())?.sequence).toBe(1);
});

test("wall-clock age expires a group stalled before its first frame", async () => {
	const clock = mockMonotonicTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ maxAge: 5000 });
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
	// so every frame of every group arrives and each group *ends* at its boundary.
	//
	// The reader is always a little behind the edge (that is what reading live means),
	// and it is parked at its group's end when the next one opens, because a group's
	// close and the next group's first frame are separate events.
	const producer = new TrackProducer("test").accept({ maxAge: 60_000 });
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

		// Parked at the end of the current group, with no close yet.
		const end = reading.readFrame();

		// The next keyframe opens its group. The verdict is taken in this window, before
		// the previous group's close lands: give the parked read a real turn to run.
		const next = producer.appendGroup();
		next.writeFrame({ payload: enc.encode("key"), timestamp: Timestamp.fromMillis(n * 2000) });
		await settle();
		open.close();
		next.writeFrame({ payload: enc.encode("tail"), timestamp: Timestamp.fromMillis(n * 2000 + 1900) });

		await expect(end).resolves.toBeUndefined();

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
	const producer = new TrackProducer("test").accept({ maxAge: 60_000 });
	const track = producer.subscribe({ maxAge: 1000 });

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
	expect(frame?.timestamp.asMillis()).toBe(1950);
});

test("a handed-out group still expires while its first frame is stalled", async () => {
	const clock = mockMonotonicTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ maxAge: 5000 });
		const track = producer.subscribe();
		producer.appendGroup();

		const stalled = await track.recvGroup();
		expect(stalled?.sequence).toBe(0);
		if (!stalled) throw new Error("missing stalled group");
		const pending = stalled.readFrame();
		const closed = stalled.closed;

		clock.set(11_000);
		producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1000) });

		// It ends rather than fails: the reader took every frame the group ever had
		// (none), so nothing was truncated. What it was waiting for was the producer,
		// and a group abandoned where its reader stands looks like one that ended there.
		await expect(pending).resolves.toBeUndefined();
		expect(await closed).toBeNull();
	} finally {
		clock.restore();
	}
});

test("committing track info wakes a group newly outside the retention window", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxAge: 10_000 });
	const source = producer.appendGroup();
	source.writeFrame({ payload: enc.encode("old"), timestamp: Timestamp.fromMillis(0) });

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect((await group.readFrame())?.timestamp.asMillis()).toBe(0);
	const waiting = group.readFrame();

	producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1_000) });
	await settle();
	producer.accept({ maxAge: 100 });

	await expect(waiting).resolves.toBeUndefined();
});

test("a handed-out frame cancels its in-flight operation when it expires", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 5000 });
	const track = producer.subscribe();
	producer.writeString("old");

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect(await group.readString()).toBe("old");

	let release!: () => void;
	const operation = new Promise<void>((resolve) => {
		release = resolve;
	});
	const guarded = hooks.guardGroup(group, operation);
	producer.writeString("new");

	await expect(guarded).rejects.toThrow("latency budget");
	release();
});

test("a guarded write keeps the position of the frame removed from the buffer", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 100 });
	const track = producer.subscribe({ maxAge: 100 });
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
	const guarded = hooks.guardGroup(group, operation, read.position);

	producer.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1_000) });
	await expect(guarded).rejects.toThrow("latency budget");
	read.complete();
	release();
});

test("clean source closure stays provisional while a frame write can expire", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 100 });
	const track = producer.subscribe({ maxAge: 100 });
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
	const guarded = hooks.guardGroup(group, operation, read.position);

	const edge = producer.appendGroup();
	edge.writeFrame({ payload: enc.encode("edge"), timestamp: Timestamp.fromMillis(1_000) });
	await expect(guarded).rejects.toThrow("latency budget");
	expect(await closed).toBeInstanceOf(Error);

	read.complete();
	release();
});

test("a drained group finishes cleanly after the live edge advances", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 5000 });
	const track = producer.subscribe();
	producer.writeString("old");

	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");
	expect(await group.readString()).toBe("old");

	producer.writeString("new");

	expect(await group.readFrame()).toBeUndefined();
	expect(group.done).toBe(true);
});

test("retention eviction preserves expiry for a handed-out group", async () => {
	const clock = mockMonotonicTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ maxAge: 100 });
		const track = producer.subscribe({ maxAge: 100 });
		const source = producer.appendGroup();
		source.writeString("first");
		source.writeString("tail");
		source.close();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(new TextDecoder().decode((await group.readFrame())?.payload)).toBe("first");

		clock.set(10_200);
		producer.appendGroup();

		await expect(group.readFrame()).rejects.toThrow("latency budget");
	} finally {
		clock.restore();
	}
});

test("retention pruning aborts a held mirror without a new live edge", async () => {
	const clock = mockMonotonicTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ maxAge: 100 });
		const track = producer.subscribe({ maxAge: 100 });
		const source = producer.appendGroup();
		source.writeString("first");
		source.writeString("tail");
		source.close();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(new TextDecoder().decode((await group.readFrame())?.payload)).toBe("first");

		clock.set(10_200);
		producer.subscribe({ maxAge: 100 });

		await expect(group.readFrame()).rejects.toBeInstanceOf(Lagged);
	} finally {
		clock.restore();
	}
});

test("retention pruning preserves clean EOF for a drained mirror", async () => {
	const clock = mockMonotonicTime(10_000);
	try {
		const producer = new TrackProducer("test").accept({ maxAge: 100 });
		const track = producer.subscribe({ maxAge: 100 });
		const source = producer.appendGroup();
		source.writeString("only");
		source.close();

		const group = await track.recvGroup();
		if (!group) throw new Error("missing group");
		expect(await group.readString()).toBe("only");
		expect(await group.readFrame()).toBeUndefined();

		clock.set(10_200);
		producer.subscribe({ maxAge: 100 });

		expect(await group.readFrame()).toBeUndefined();
		expect(await group.closed).toBeNull();
	} finally {
		clock.restore();
	}
});

test("system clock changes do not affect wall-clock latency", async () => {
	const clock = mockMonotonicTime(10_000);
	setSystemTime(new Date(10_000));
	try {
		const producer = new TrackProducer("test").accept({ maxAge: 100 });
		const track = producer.subscribe({ maxAge: 100 });
		producer.writeString("old");

		setSystemTime(new Date(20_000));
		producer.writeString("new");

		expect((await track.recvGroup())?.sequence).toBe(0);
		expect((await track.recvGroup())?.sequence).toBe(1);
	} finally {
		setSystemTime();
		clock.restore();
	}
});

test("frame readiness cancels the losing latency waiter", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 5000 });
	const track = producer.subscribe({ maxAge: 5000 });
	const source = producer.appendGroup();
	const group = await track.recvGroup();
	if (!group) throw new Error("missing group");

	for (let i = 0; i < 110; i++) {
		const pending = group.readFrame();
		source.writeString(`${i}`);
		expect(new TextDecoder().decode((await pending)?.payload)).toBe(`${i}`);
	}
});

test("latency budget retains a consumed live-edge anchor", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 5000 });
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
	const producer = new TrackProducer("test").accept({ maxAge: 5000 });
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

test("endAt caps the live edge used by the latency budget", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 5000 });
	const track = producer.subscribe();
	track.endAt(1);

	for (const timestamp of [0, 1000, 2000]) {
		producer.writeFrame({ payload: enc.encode(`${timestamp}`), timestamp: Timestamp.fromMillis(timestamp) });
	}

	expect((await track.recvGroup())?.sequence).toBe(1);
	track.endAt(undefined);
	expect((await track.recvGroup())?.sequence).toBe(2);
});

test("endAt does not cap frame-level reads", async () => {
	const producer = new TrackProducer("test").accept({ maxAge: 5000 });
	const track = producer.subscribe({ maxAge: 5000 });
	track.endAt(0);

	producer.writeFrame({ payload: enc.encode("zero"), timestamp: Timestamp.fromMillis(0) });
	producer.writeFrame({ payload: enc.encode("one"), timestamp: Timestamp.fromMillis(1) });
	producer.close();

	expect((await track.readFrameSequence())?.group).toBe(0);
	expect((await track.readFrameSequence())?.group).toBe(1);
	expect(await track.readFrameSequence()).toBeUndefined();
});

test("local cursor bounds can skip, pause, and release buffered groups", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxAge: 5000 });

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

// A relay can ingest back-to-back groups micro-reordered (the upstream leg sends
// newest-first). The older group is cached and in demand, so serving must still
// deliver it; a sequence cursor would skip it permanently.
test("recvGroup serves a late arrival after a newer group", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxAge: 5000 });

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

// recvGroup honors the endAt cap by parking, like nextGroup: beyond-cap groups are
// held, not dropped, and a raised cap re-offers them, even after a clean close.
test("endAt parks recvGroup beyond the cap and a raised cap re-offers", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxAge: 5000 });

	for (let sequence = 0; sequence < 3; sequence++) producer.writeGroup(new GroupProducer(sequence));

	track.endAt(1);
	expect((await track.recvGroup())?.sequence).toBe(0);
	expect((await track.recvGroup())?.sequence).toBe(1);

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	// A clean close keeps the parked group claimable: the cap may still rise.
	producer.close();
	expect(await Promise.race([pending, new Promise((resolve) => setTimeout(() => resolve("parked"), 10))])).toBe(
		"parked",
	);

	track.endAt();
	expect((await pending)?.sequence).toBe(2);
	expect(await track.recvGroup()).toBeUndefined();
});

// A group beyond the cap must not block in-range groups that arrive behind it:
// a relay can ingest a burst micro-reordered (newest first).
test("recvGroup serves in-range groups that arrive behind a capped one", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxAge: 5000 });

	track.endAt(1);

	// Reordered burst: the beyond-cap group arrives first.
	producer.writeGroup(new GroupProducer(2));
	producer.writeGroup(new GroupProducer(0));
	producer.writeGroup(new GroupProducer(1));

	expect((await track.recvGroup())?.sequence).toBe(0);
	expect((await track.recvGroup())?.sequence).toBe(1);

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.endAt(2);
	expect((await pending)?.sequence).toBe(2);
});

// A raised startAt drops parked groups it overtook instead of re-offering them
// once the cap rises.
test("startAt drops groups recvGroup parked at the cap", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe();

	track.endAt(0);
	producer.writeGroup(new GroupProducer(1));
	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.startAt(2);
	track.endAt();
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
	first.writeString("first");
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
	retry.writeString("retry");
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

	track.endAt(0);
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

	track.endAt(0);
	producer.writeGroup(new GroupProducer(1));

	const pending = track.recvGroup();
	expect(await Promise.race([pending, Promise.resolve("pending")])).toBe("pending");

	track.close();
	expect(await pending).toBeUndefined();
});

test("recvGroup after nextGroup still returns late arrivals", async () => {
	const producer = new TrackProducer("test");
	const track = producer.subscribe({ maxAge: 5000 });

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
	const track = producer.subscribe({ maxAge: 5000 });

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
