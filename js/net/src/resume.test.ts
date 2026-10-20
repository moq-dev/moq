import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import * as Epoch from "./epoch.ts";
import { NotFound, StreamCode, StreamError } from "./error.ts";
import { Producer as GroupProducer } from "./group.ts";
import { Route } from "./hop.ts";
import { hooks } from "./internal.ts";
import { Producer } from "./origin.ts";
import * as Path from "./path.ts";
import { Front } from "./resume.ts";
import type * as track from "./track.ts";
import { overrideBroadcastWire, wireOf } from "./wire.ts";

const EPOCH = Epoch.parse("01900000-0000-7000-8000-000000000001");
const path = Path.from("live");
const enc = new TextEncoder();
const dec = new TextDecoder();

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

/** A stand-in session announcing `path` under the epoch at `cost`, serving `source`. */
function serve(origin: Producer, source: BroadcastProducer, cost: bigint) {
	const handle = wireOf(origin).receive(path, Route.normalize({ epoch: EPOCH, cost }));
	void (async () => {
		for await (const request of handle.requested()) request.accept(source.consume());
	})();
	return () => handle.close();
}

/** A copy of the logical track: every route's copy holds the same groups under the same sequences. */
function copy(source: BroadcastProducer) {
	const producer = source.createTrack("seq");
	return {
		producer,
		write(sequence: number, frames = 1, close = true) {
			const group = new GroupProducer(sequence);
			producer.writeGroup(group);
			for (let frame = 0; frame < frames; frame++)
				group.writeFrame({ payload: enc.encode(`${sequence}.${frame}`) });
			if (close) group.close();
			return group;
		},
	};
}

/** Every group sequence readable without waiting. */
function drain(sub: track.Subscriber): number[] {
	const sequences: number[] = [];
	for (let group = sub.tryRecvGroup(); group; group = sub.tryRecvGroup()) {
		sequences.push(group.sequence);
		group.close();
	}
	return sequences;
}

async function setup() {
	const origin = new Producer();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const disposeA = serve(origin, a, 5n);
	const request = origin.consume().request(path);
	await settle();
	const active = request.active.peek();
	if (!active) throw new Error("expected the path to resolve");
	return {
		origin,
		a,
		b,
		disposeA,
		request,
		active,
		// The cheaper route takes over once it answers.
		swap: async () => {
			const disposeB = serve(origin, b, 1n);
			await settle();
			await settle();
			return disposeB;
		},
		close: () => {
			request.close();
			disposeA();
			a.close();
			b.close();
			origin.close();
		},
	};
}

test("a subscription reads on across a same-epoch route swap, each group once", async () => {
	const { a, b, active, request, swap, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe({ maxDelay: 10_000 as track.Subscription["maxDelay"] });
	ta.write(0);
	expect(drain(sub)).toEqual([0]);

	// Both routes deliver group 1; the reader takes it once.
	ta.write(1);
	tb.write(0);
	tb.write(1);
	await swap();
	expect(request.active.peek()).toBe(active);
	tb.write(2);
	expect(drain(sub)).toEqual([1, 2]);

	// The replaced route's later groups are the new route's to deliver.
	ta.write(3);
	expect(drain(sub)).toEqual([]);
	tb.write(3);
	expect(drain(sub)).toEqual([3]);

	sub.close();
	close();
});

test("a stalled group continues from the replacement at the next frame", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	ta.write(0, 1, false);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");

	// The new route has the rest of it; the old one stalls.
	tb.write(0, 3);
	await swap();
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.1");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.2");
	expect(await group.readFrame()).toBeUndefined();

	sub.close();
	close();
});

test("a group whose route fails continues from the replacement", async () => {
	const { a, b, swap, active, disposeA, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	const stalled = ta.write(0, 1, false);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");

	tb.write(0, 2);
	await swap();
	// The old route's session goes, failing what it was still delivering.
	stalled.close(new StreamError(StreamCode.SessionClosed));
	disposeA();
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.1");
	expect(await group.readFrame()).toBeUndefined();

	sub.close();
	close();
});

test("a group no route continues is given up once a newer one supersedes it", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	const stalled = ta.write(0, 1, false);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	await group.readFrame();

	// The new route never had group 0, and its group 1 is past a zero budget's reach for it.
	tb.write(1);
	await swap();
	stalled.close(new StreamError(StreamCode.SessionClosed));
	await expect(group.readFrame()).rejects.toBeInstanceOf(StreamError);
	expect(drain(sub)).toEqual([1]);

	sub.close();
	close();
});

test("a replacement claiming different track properties is refused once the old copy runs out", async () => {
	const { a, b, swap, active, close } = await setup();
	const old = a.createTrack("seq", { priority: 1 });
	b.createTrack("seq", { priority: 2 });
	const sub = active.track("seq").subscribe();
	expect((await sub.info()).priority).toBe(1);
	await swap();
	expect(sub.closed.peek()).toBeUndefined();

	// The old copy failing with its route is what ends the track.
	old.close(new StreamError(StreamCode.SessionClosed));
	await settle();
	expect(() => sub.tryRecvGroup()).toThrow("changed its properties");

	sub.close();
	close();
});

test("waiting for the end of a track whose replacement was refused ends with the refusal", async () => {
	const { a, b, swap, active, close } = await setup();
	const old = a.createTrack("seq", { priority: 1 });
	b.createTrack("seq", { priority: 2 });
	const sub = active.track("seq").subscribe();
	expect((await sub.info()).priority).toBe(1);
	await swap();
	const finished = sub.finished();
	finished.catch(() => {});

	// The old copy failing with its route leaves only the refusal.
	old.close(new StreamError(StreamCode.SessionClosed));
	await expect(finished).rejects.toThrow("changed its properties");

	sub.close();
	close();
});

test("a fetched group continued from an empty, already-closed copy releases that route once", async () => {
	const errors: unknown[] = [];
	const onError = (err: unknown) => errors.push(err);
	process.on("unhandledRejection", onError);
	try {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const c = new BroadcastProducer();
		copy(a);
		copy(b);
		copy(c);
		front.serve(a.consume());
		// A reader on another track holds each route's broadcast too.
		const sub = front.consumer.track("seq").subscribe();

		// Group 5 is empty: the first route fails it, the next holds it closed.
		const failing = new GroupProducer(5);
		a.createTrack("fetch").writeGroup(failing);
		const group = await front.consumer.track("fetch").fetchGroup(5);
		const read = group.readFrame();
		const whole = new GroupProducer(5);
		b.createTrack("fetch").writeGroup(whole);
		whole.close();
		front.serve(b.consume());
		await settle();
		failing.close(new StreamError(StreamCode.SessionClosed));
		expect(await read).toBeUndefined();
		group.close();
		await settle();

		// Moving off the route and closing everything gives back every hold, and no more.
		front.serve(c.consume());
		await settle();
		sub.close();
		front.consumer.close();
		await settle();
		a.close();
		b.close();
		c.close();
	} finally {
		process.off("unhandledRejection", onError);
	}
	expect(errors).toEqual([]);
});

test("a fetched group failing as it is handed out releases its route once", async () => {
	// The failure lands at each turn around the hand-out, so one lands between the fetch
	// returning and its recovery being attached.
	for (let turns = 0; turns < 24; turns++) {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		front.serve(a.consume());
		// A reader on another track of the same route holds the route's broadcast too.
		const sub = front.consumer.track("seq").subscribe();
		ta.write(0);
		expect(drain(sub)).toEqual([0]);

		const fa = a.createTrack("fetch");
		const failing = new GroupProducer(5);
		fa.writeGroup(failing);
		failing.writeFrame({ payload: enc.encode("5.0") });
		const fetching = front.consumer.track("fetch").fetchGroup(5);
		for (let turn = 0; turn < turns; turn++) await Promise.resolve();
		failing.close(new StreamError(StreamCode.SessionClosed));
		await settle();

		// The next route holds the fetched group whole; the old one still feeds the reader a
		// group the new route never has.
		const fb = b.createTrack("fetch");
		const whole = new GroupProducer(5);
		fb.writeGroup(whole);
		whole.writeFrame({ payload: enc.encode("5.0") });
		whole.writeFrame({ payload: enc.encode("5.1") });
		whole.close();
		b.createTrack("seq");
		front.serve(b.consume());
		const group = await fetching;
		const frames: string[] = [];
		for (let frame = await group.readFrame(); frame; frame = await group.readFrame())
			frames.push(dec.decode(frame.payload));
		expect(frames).toEqual(["5.0", "5.1"]);
		group.close();
		await settle();

		ta.write(1);
		expect(drain(sub)).toEqual([1]);

		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("datagrams a new copy replays are handed out once", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	ta.producer.appendDatagram(undefined, enc.encode("a0"));
	expect(dec.decode((await sub.recvDatagram())?.payload)).toBe("a0");

	tb.producer.appendDatagram(undefined, enc.encode("b0"));
	await swap();
	tb.producer.appendDatagram(undefined, enc.encode("b1"));
	expect(dec.decode((await sub.recvDatagram())?.payload)).toBe("b1");

	sub.close();
	close();
});

test("a route that goes for good ends the track as its last copy does", async () => {
	const { a, active, disposeA, request, close } = await setup();
	const ta = copy(a);
	const sub = active.track("seq").subscribe();
	ta.write(0);
	expect(drain(sub)).toEqual([0]);

	ta.producer.close(new StreamError(StreamCode.SessionClosed));
	disposeA();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	await expect(sub.recvGroup()).rejects.toBeInstanceOf(StreamError);

	close();
});

test("between routes a reader keeps reading the copy it has", async () => {
	const { origin, a, b, active, request, disposeA, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	ta.write(0);
	expect(drain(sub)).toEqual([0]);

	// The next route has not answered when the old one is retracted.
	const next = wireOf(origin).receive(path, Route.normalize({ epoch: EPOCH, cost: 1n }));
	const asked = next.requested();
	disposeA();
	await settle();
	expect(request.active.peek()).toBe(active);
	expect(drain(sub)).toEqual([]);
	ta.write(1);
	expect(drain(sub)).toEqual([1]);

	const { value: pending } = await asked.next();
	pending?.accept(b.consume());
	await settle();
	await settle();
	tb.write(1);
	tb.write(2);
	expect(drain(sub)).toEqual([2]);

	sub.close();
	next.close();
	close();
});

test("a parked front keeps its readers on the copy they have", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	expect(drain(sub)).toEqual([0]);

	// Nothing still reads group 0 from it.
	await settle();
	front.park();
	expect(drain(sub)).toEqual([]);
	ta.write(1);
	expect(drain(sub)).toEqual([1]);

	// Spliced in once its copy proves compatible.
	front.serve(b.consume());
	await settle();
	tb.write(1);
	tb.write(2);
	expect(drain(sub)).toEqual([2]);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a fetched group continues from the next route once its own fails", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	const stalled = ta.write(5, 1, false);
	front.serve(a.consume());
	const group = await front.consumer.track("seq").fetchGroup(5);
	expect(dec.decode((await group.readFrame())?.payload)).toBe("5.0");

	tb.write(5, 2);
	front.serve(b.consume());
	stalled.close(new StreamError(StreamCode.SessionClosed));
	expect(dec.decode((await group.readFrame())?.payload)).toBe("5.1");
	expect(await group.readFrame()).toBeUndefined();

	front.consumer.close();
	a.close();
	b.close();
});

test("a replacement spliced before the first copy answers is still checked against it", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const old = a.createTrack("seq", { priority: 1 });
	b.createTrack("seq", { priority: 2 });
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	// Swapped in the same turn, before the first copy's info committed anywhere.
	front.serve(b.consume());
	await settle();
	expect((await sub.info()).priority).toBe(1);

	old.close(new StreamError(StreamCode.SessionClosed));
	await settle();
	expect(() => sub.tryRecvGroup()).toThrow("changed its properties");

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("an ordered reader takes a replaced copy's buffered group before the new copy's later one", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const ordered = front.consumer
		.track("seq")
		.subscribe({ maxDelay: 10_000 as track.Subscription["maxDelay"] })
		.ordered();
	ta.write(0);
	expect((await ordered.nextGroup())?.sequence).toBe(0);

	// The old copy holds group 1 unread; the new one only has group 2.
	ta.write(1);
	tb.write(2);
	front.serve(b.consume());
	await settle();
	expect((await ordered.nextGroup())?.sequence).toBe(1);
	expect((await ordered.nextGroup())?.sequence).toBe(2);

	ordered.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a late subscriber's first route is checked against properties an earlier source gave", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	a.createTrack("seq", { priority: 1 });
	b.createTrack("seq", { priority: 2 });
	front.serve(a.consume());
	expect((await front.consumer.track("seq").info()).priority).toBe(1);

	// Nobody reads the track when the route moves, so nothing has been spliced yet.
	front.serve(b.consume());
	const sub = front.consumer.track("seq").subscribe();
	await settle();
	expect(() => sub.tryRecvGroup()).toThrow("changed its properties");

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a recovery fetch a later route supersedes is abandoned", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const c = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	const tc = copy(c);
	const stalled = ta.write(0, 1, false);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");

	// The next route does not have the group yet: recovery fetches it there and waits.
	front.serve(b.consume());
	await settle();
	stalled.close(new StreamError(StreamCode.SessionClosed));
	const read = group.readFrame();
	await settle();
	const fetching = tb.producer.demand().wanted.peek() !== undefined;

	// Another route has it cached: the group continues there, and the fetch from b stops.
	tc.write(0, 2);
	front.serve(c.consume());
	expect(dec.decode((await read)?.payload)).toBe("0.1");
	sub.close();
	await settle();
	expect(fetching).toBe(true);
	expect(tb.producer.demand().wanted.peek()).toBeUndefined();

	front.consumer.close();
	a.close();
	b.close();
	c.close();
});

test("track info keeps one answer across routes", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	a.createTrack("seq", { priority: 1 });
	b.createTrack("seq", { priority: 2 });
	front.serve(a.consume());
	expect((await front.consumer.track("seq").info()).priority).toBe(1);
	front.serve(b.consume());
	await expect(front.consumer.track("seq").info()).rejects.toThrow("changed its properties");

	front.consumer.close();
	a.close();
	b.close();
});

test("a datagram reader lets go of the copies a swap replaced", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	expect(ta.producer.demand().wanted.peek()).toBeDefined();
	front.serve(b.consume());
	await settle();
	// A read with nothing to deliver is the pass that lets go of a's copy.
	void sub.recvDatagram();
	await settle();
	expect(ta.producer.demand().wanted.peek()).toBeUndefined();

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a front remembers it served a session's broadcast between routes", () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const session = a.consume();
	overrideBroadcastWire(session, {});
	front.serve(session);
	expect(front.fromSession).toBe(true);
	front.park();
	expect(front.fromSession).toBe(true);
	front.consumer.close();
	a.close();
});

test("a failed group on a route whose replacement was refused ends with the refusal", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = a.createTrack("seq", { priority: 1 });
	b.createTrack("seq", { priority: 2 });
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	const stalled = new GroupProducer(0);
	ta.writeGroup(stalled);
	stalled.writeFrame({ payload: enc.encode("0.0") });
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	await group.readFrame();
	await sub.info();

	front.serve(b.consume());
	await settle();
	stalled.close(new StreamError(StreamCode.SessionClosed));
	await expect(group.readFrame()).rejects.toThrow("changed its properties");

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("draining groups to their end leaves buffered datagrams readable", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	ta.producer.appendDatagram(undefined, enc.encode("d"));
	ta.producer.close();
	expect(drain(sub)).toEqual([0]);
	expect(await sub.recvGroup()).toBeUndefined();
	expect(dec.decode((await sub.recvDatagram())?.payload)).toBe("d");

	sub.close();
	front.consumer.close();
	a.close();
});

test("a datagram read keeps a replaced copy a group reader still needs", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ maxDelay: 10_000 as track.Subscription["maxDelay"] });
	ta.write(0);
	expect(drain(sub)).toEqual([0]);
	ta.write(1);

	front.serve(b.consume());
	await settle();
	void sub.recvDatagram();
	expect(drain(sub)).toEqual([1]);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a fetch never reads a replacement refused for its track properties", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	a.createTrack("seq", { priority: 1 });
	const tb = b.createTrack("seq", { priority: 2 });
	const group = new GroupProducer(0);
	tb.writeGroup(group);
	group.writeFrame({ payload: enc.encode("b") });
	group.close();
	front.serve(a.consume());
	await front.consumer.track("seq").info();
	front.serve(b.consume());
	await expect(front.consumer.track("seq").fetchGroup(0)).rejects.toThrow("changed its properties");

	front.consumer.close();
	a.close();
	b.close();
});

test("the first fetch of a track says what the track is for later routes", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	for (const [source, priority] of [
		[a, 1],
		[b, 2],
	] as const) {
		const producer = source.createTrack("seq", { priority });
		const group = new GroupProducer(0);
		producer.writeGroup(group);
		group.writeFrame({ payload: enc.encode(`${priority}`) });
		group.close();
	}
	front.serve(a.consume());
	(await front.consumer.track("seq").fetchGroup(0)).close();
	front.serve(b.consume());
	await expect(front.consumer.track("seq").fetchGroup(0)).rejects.toThrow("changed its properties");

	front.consumer.close();
	a.close();
	b.close();
});

test("aborting a fetch stops it while its route's properties are still being checked", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	a.createTrack("seq");
	// b serves tracks on demand but never answers, so its properties never arrive.
	void wireOf(b).requested();
	front.serve(a.consume());
	await front.consumer.track("seq").info();
	front.serve(b.consume());
	const abort = new AbortController();
	const fetching = front.consumer.track("seq").fetchGroup(0, { signal: abort.signal });
	await settle();
	abort.abort(new Error("cancelled"));
	await expect(fetching).rejects.toThrow("cancelled");

	front.consumer.close();
	a.close();
	b.close();
});

test("a refused route is not served again by a later compatible source", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const c = new BroadcastProducer();
	const ta = copy(a);
	b.createTrack("seq", { priority: 2 });
	const tc = copy(c);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	expect(drain(sub)).toEqual([0]);
	front.serve(b.consume());
	await settle();
	front.serve(c.consume());
	await settle();
	tc.write(1);
	expect(drain(sub)).toEqual([]);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
	c.close();
});

// A replaced copy is bounded only once the new copy delivers its first group: one the old route
// gets after the swap, which the new route never has, still arrives.
test("a group the old route gets after the swap, which the new route never has, is delivered", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	expect(drain(sub)).toEqual([0]);
	front.serve(b.consume());
	await settle();
	// Group 1 reaches the old route after the swap; the new route joins at group 2.
	ta.write(1);
	expect(drain(sub)).toEqual([1]);
	tb.write(2);
	expect(drain(sub)).toEqual([2]);
	// Bounded at the group before the new copy's first: later groups come from the new route.
	ta.write(3);
	expect(drain(sub)).toEqual([]);
	tb.write(3);
	expect(drain(sub)).toEqual([3]);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a group the new route replays bounds nothing, so the old route's next group still arrives", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	expect(drain(sub)).toEqual([0]);
	ta.write(1);
	front.serve(b.consume());
	await settle();
	// The new route only replays group 0, which was already handed out.
	tb.write(0);
	expect(drain(sub)).toEqual([1]);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test.each([true, false])(
	"a group every route holds comes from the serving one, so replaced copies go (ordered: %s)",
	async (ordered) => {
		const front = new Front();
		const sources = Array.from({ length: 4 }, () => new BroadcastProducer());
		const copies = sources.map(copy);
		front.serve(sources[0].consume());
		const sub = front.consumer.track("seq").subscribe();
		const view = ordered ? sub.ordered() : undefined;
		const next = async () => {
			const group = await (view ? view.nextGroup() : sub.recvGroup());
			group?.close();
			return group?.sequence;
		};
		copies[0].write(0);
		expect(await next()).toBe(0);
		let pending = next();
		for (let route = 1; route < sources.length; route++) {
			front.serve(sources[route].consume());
			await settle();
			// Every route so far holds the next group.
			for (const held of copies.slice(0, route + 1)) held.write(route);
			expect(await pending).toBe(route);
			// The serving copy's group bounded the previous route's copy, so the next read lets it go.
			pending = next();
			await settle();
			expect(copies[route - 1].producer.demand().wanted.peek()).toBeUndefined();
		}

		view?.close();
		sub.close();
		front.consumer.close();
		for (const source of sources) source.close();
	},
);

test.each([
	[true, true],
	[false, true],
	[true, false],
	[false, false],
])(
	"a group the serving copy failed continues from a replaced copy that holds it (ordered: %p, failed before handout: %p)",
	async (ordered, early) => {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe();
		const view = ordered ? sub.ordered() : undefined;
		const next = () => (view ? view.nextGroup() : sub.recvGroup());
		ta.write(0);
		(await next())?.close();
		front.serve(b.consume());
		await settle();

		// Both routes hold group 1: the old one whole, the serving one failing after a frame.
		ta.write(1, 2);
		const failing = tb.write(1, 1, false);
		const fail = () => failing.close(new StreamError(StreamCode.SessionClosed));
		if (early) fail();
		const group = await next();
		if (group?.sequence !== 1) throw new Error("expected group 1");
		// Reading on must not let go of the replaced copy while the group still needs it.
		const later = next();
		await settle();
		if (!early) fail();
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.1");
		expect(await group.readFrame()).toBeUndefined();

		group.close();
		tb.write(2);
		expect((await later)?.sequence).toBe(2);
		view?.close();
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	},
);

test.each([true, false])(
	"a replaced copy whose route fails still backs a group handed out while it held it (ordered: %p)",
	async (ordered) => {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe();
		const view = ordered ? sub.ordered() : undefined;
		const next = () => (view ? view.nextGroup() : sub.recvGroup());
		ta.write(0);
		(await next())?.close();
		front.serve(b.consume());
		await settle();

		ta.write(1, 2);
		const failing = tb.write(1, 1, false);
		const group = await next();
		if (group?.sequence !== 1) throw new Error("expected group 1");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.0");

		// The old route goes away, and the reader reads on and sees its copy end.
		ta.producer.close(new StreamError(StreamCode.SessionClosed));
		const later = next();
		await settle();
		failing.close(new StreamError(StreamCode.SessionClosed));
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.1");
		expect(await group.readFrame()).toBeUndefined();

		group.close();
		tb.write(2);
		expect((await later)?.sequence).toBe(2);
		view?.close();
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	},
);

test("a fetched group that fails on the route still serving waits for the next route", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	const failing = ta.write(5, 1, false);
	front.serve(a.consume());
	const group = await front.consumer.track("seq").fetchGroup(5);
	expect(dec.decode((await group.readFrame())?.payload)).toBe("5.0");

	// Its route stays current, so its only copy of group 5 is the failed one.
	failing.close(new StreamError(StreamCode.SessionClosed));
	const read = group.readFrame();
	// Timers still run: recovery does not spin on the failed copy.
	await settle();
	await settle();

	tb.write(5, 2);
	front.serve(b.consume());
	expect(dec.decode((await read)?.payload)).toBe("5.1");
	expect(await group.readFrame()).toBeUndefined();

	front.consumer.close();
	a.close();
	b.close();
});

test("a group handed out already failed with its route still continues on the next one", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	// The old route fails group 0 before the reader takes it.
	const failed = ta.write(0, 1, false);
	failed.close(new StreamError(StreamCode.SessionClosed));
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
	const read = group.readFrame();

	tb.write(0, 2);
	front.serve(b.consume());
	expect(dec.decode((await read)?.payload)).toBe("0.1");

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("track info asks the next route when the current one never answers", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	// a serves tracks on demand but never answers.
	void wireOf(a).requested();
	b.createTrack("seq", { priority: 2 });
	front.serve(a.consume());
	const info = front.consumer.track("seq").info();
	await settle();
	front.serve(b.consume());
	expect((await info).priority).toBe(2);

	front.consumer.close();
	a.close();
	b.close();
});

test("a route superseded while its properties are checked stops being asked", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const c = new BroadcastProducer();
	copy(a);
	copy(c);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	await sub.info();
	// b never answers what the track is, so its splice waits on it.
	void wireOf(b).requested();
	front.serve(b.consume());
	await settle();
	expect(b.demand().used.peek()).toBe(true);

	front.serve(c.consume());
	await settle();
	expect(b.demand().used.peek()).toBe(false);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
	c.close();
});

test("track info waits for the copy being read to say what the track is", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	// a answers what the track is only when told to.
	const requests = wireOf(a).requested();
	b.createTrack("seq", { priority: 2 });
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	front.serve(b.consume());
	const info = front.consumer.track("seq").info();
	info.catch(() => {});
	await settle();

	const request = await requests;
	request?.accept({ priority: 1 });
	await expect(info).rejects.toThrow("changed its properties");
	expect((await sub.info()).priority).toBe(1);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a refusal from the copy being read is the track's answer, for its readers and fetches alike", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	// a refuses the track only when told to.
	const requests = wireOf(a).requested();
	const tb = copy(b);
	tb.write(0);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	front.serve(b.consume());
	const fetching = front.consumer.track("seq").fetchGroup(0);
	fetching.catch(() => {});
	await settle();

	const request = await requests;
	request?.reject(new NotFound("seq"));
	await expect(fetching).rejects.toBeInstanceOf(NotFound);
	await expect(sub.recvGroup()).rejects.toBeInstanceOf(NotFound);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a fetch waits for the copy being read to say what the track is", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	// a answers what the track is only when told to.
	const requests = wireOf(a).requested();
	const tb = b.createTrack("seq", { priority: 2 });
	const group = new GroupProducer(0);
	tb.writeGroup(group);
	group.writeFrame({ payload: enc.encode("b") });
	group.close();
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	front.serve(b.consume());
	const fetching = front.consumer.track("seq").fetchGroup(0);
	fetching.catch(() => {});
	await settle();

	const request = await requests;
	request?.accept({ priority: 1 });
	await expect(fetching).rejects.toThrow("changed its properties");

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("an arrival read takes a replaced copy's buffered group before the new copy's later one", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ maxDelay: 10_000 as track.Subscription["maxDelay"] });
	ta.write(0);
	expect(drain(sub)).toEqual([0]);
	// Both buffered when the read starts: the lower one comes first.
	ta.write(1);
	tb.write(2);
	front.serve(b.consume());
	await settle();
	expect(drain(sub)).toEqual([1, 2]);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a fetch still out on the old route moves to the new one", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	// a has the track but not group 5 yet, so its fetch waits.
	a.createTrack("seq");
	const tb = copy(b);
	tb.write(5);
	front.serve(a.consume());
	const fetching = front.consumer.track("seq").fetchGroup(5);
	await settle();
	front.serve(b.consume());
	const group = await fetching;
	expect(dec.decode((await group.readFrame())?.payload)).toBe("5.0");

	group.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("the largest follows the serving copy across a swap without moving back, and ends with the route", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(3);
	expect(await sub.largest()).toEqual({ group: 3, frame: 0 });
	// A publisher serving the track peeks the same edge.
	expect(hooks.peekLargest(sub).largest).toEqual({ group: 3, frame: 0 });

	// The route fails: its copy's stream ends, the logical track's does not.
	ta.producer.close(new StreamError(StreamCode.SessionClosed));
	let settled = false;
	const next = sub.largest().finally(() => {
		settled = true;
	});
	await settle();
	expect(settled).toBe(false);

	// The replacement starts behind what is already known.
	tb.write(2);
	front.serve(b.consume());
	await settle();
	expect(settled).toBe(false);
	tb.write(4);
	expect(await next).toEqual({ group: 4, frame: 0 });

	// No route replaces this one: the stream ends with its copy.
	front.conclude();
	const last = sub.largest();
	tb.producer.close();
	expect(await last).toBeUndefined();

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});
