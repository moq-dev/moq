import { expect, test } from "bun:test";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import * as Epoch from "./epoch.ts";
import { StreamCode, StreamError } from "./error.ts";
import { Producer as GroupProducer } from "./group.ts";
import { Route } from "./hop.ts";
import { Producer } from "./origin.ts";
import * as Path from "./path.ts";
import { Front } from "./resume.ts";
import type * as track from "./track.ts";
import { wireOf } from "./wire.ts";

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
