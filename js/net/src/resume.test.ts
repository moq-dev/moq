import { expect, spyOn, test } from "bun:test";
import { Manual, Milli, Nano, yieldNow } from "@moq/clock";
import { Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "./broadcast.ts";
import * as Epoch from "./epoch.ts";
import { NotFound, SessionCode, SessionError, StreamCode, StreamError } from "./error.ts";
import { type Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import { Route } from "./hop.ts";
import type { Session as IetfSession } from "./ietf/session.ts";
import { Subscriber as IetfSubscriber } from "./ietf/subscriber.ts";
import { hooks, MAX_DATAGRAMS } from "./internal.ts";
import type { Lister } from "./listing.ts";
import { Producer } from "./origin.ts";
import * as Path from "./path.ts";
import { Front, routeFailed } from "./resume.ts";
import type * as track from "./track.ts";
import { unwanted } from "./util/demand.ts";
import { overrideBroadcastWire, wireOf } from "./wire.ts";

const EPOCH = Epoch.parse("01900000-0000-7000-8000-000000000001");
const path = Path.from("live");
const enc = new TextEncoder();
const dec = new TextDecoder();

async function settle() {
	await yieldNow();
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

/** A stand-in session announcing `path` under the epoch at `cost`, serving `source`. */
function serve(origin: Producer, source: BroadcastProducer, cost: bigint) {
	const handle = wireOf(origin).receive(path, Route.normalize({ epoch: EPOCH, cost }));
	void (async () => {
		for await (const request of handle.requested()) hooks.acceptRequest(request, source.consume());
	})();
	return () => handle.close();
}

/** A copy of the logical track: every route's copy holds the same groups under the same sequences. */
function copy(source: BroadcastProducer) {
	const producer = source.createTrack("seq");
	return {
		producer,
		write(sequence: number, frames = 1, close = true) {
			const group = producer.createGroup(sequence);
			for (let frame = 0; frame < frames; frame++)
				group.writeFrame({ payload: enc.encode(`${sequence}.${frame}`) });
			if (close) group.finish();
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

test("returning to a route reuses its subscription and held group", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const tracks = sources.map(copy);
	const open = tracks[0].write(0, 1, false);
	const mirrored = spyOn(open, "mirror");
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) });
	try {
		await clock.advance(Nano.zero);
		const held = sub.tryRecvGroup();
		if (!held) throw new Error("expected the held group");
		expect(dec.decode((await held.readFrame())?.payload)).toBe("0.0");
		for (let round = 0; round < 32; round++) {
			front.serve(handles[round % 2]);
			await clock.advance(Nano.zero);
			expect(sub.tryRecvGroup()).toBeUndefined();
			expect(mirrored.mock.calls.length).toBe(1);
		}
		tracks[1].write(1);
		expect(drain(sub)).toEqual([1]);
		front.serve(handles[0]);
		await clock.advance(Nano.zero);
		open.writeFrame({ payload: enc.encode("0.1") });
		expect(dec.decode((await held.readFrame())?.payload)).toBe("0.1");
		tracks[0].write(2);
		expect(drain(sub)).toEqual([2]);
		held.close();
	} finally {
		mirrored.mockRestore();
		open.finish();
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

/**
 * A returning route resumes where a fresh copy would, and what it buffered while replaced below
 * what another route already handed out comes through the backfill: the late group once.
 */
test("a returning route backfills below the reader's newest", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	const open = ta.write(0, 1, false);
	const mirrored = spyOn(open, "mirror");
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) });
	try {
		await clock.advance(Nano.zero);
		const held = sub.tryRecvGroup();
		if (!held) throw new Error("expected the held group");
		front.serve(handles[1]);
		await clock.advance(Nano.zero);
		tb.write(10);
		tb.write(20);
		expect(drain(sub)).toEqual([10, 20]);
		ta.write(15);
		front.serve(handles[0]);
		await clock.advance(Nano.zero);
		expect(mirrored.mock.calls.length).toBe(1);
		expect(drain(sub)).toEqual([15]);
		ta.write(26);
		expect(drain(sub)).toEqual([26]);
		held.close();
	} finally {
		mirrored.mockRestore();
		open.finish();
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

/**
 * A group below the replacement's resume point that the reader never got is asked of the new
 * route, as one cursor on the logical track would still get it late.
 */
/**
 * A gap that stays open across route flaps keeps one backfill per distinct route: a later resume
 * point widens the route's backfill rather than asking again.
 */
test("route flaps keep one backfill per distinct route", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const tracks = sources.map(copy);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) });
	const subscribed = tracks.map((track) => spyOn(track.producer, "subscribe"));
	try {
		await clock.advance(Nano.zero);
		for (let round = 1; round < 24; round++) {
			front.serve(handles[round % 2]);
			await clock.advance(Nano.zero);
			// Every odd group never lands, so every resume point leaves a gap.
			tracks[round % 2].write(round * 2);
			await clock.advance(Nano.zero);
			expect(drain(sub)).toEqual([round * 2]);
		}
		// Each route's own copy, returned to on every flap, and its one backfill.
		for (const spy of subscribed) expect(spy.mock.calls.length).toBe(2);
		tracks[1].write(1);
		await clock.advance(Nano.zero);
		expect(drain(sub)).toEqual([1]);
	} finally {
		for (const spy of subscribed) spy.mockRestore();
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

test("a group below the replacement's resume point is backfilled", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) });
	try {
		await clock.advance(Nano.zero);
		ta.write(5);
		expect(drain(sub)).toEqual([5]);
		front.serve(handles[1]);
		await clock.advance(Nano.zero);
		expect(drain(sub)).toEqual([]);
		tb.write(2);
		tb.write(6);
		await clock.advance(Nano.zero);
		expect(drain(sub)).toEqual([2, 6]);
		tb.write(4);
		await clock.advance(Nano.zero);
		expect(drain(sub)).toEqual([4]);
	} finally {
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

test("a routed source start follows the serving copy and its subscriber floor", async () => {
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	hooks.startTrack(ta.producer, { start: 1 });
	hooks.startTrack(tb.producer, { start: 5, requested: 3 });
	const front = new Front();
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ groups: { start: { included: 3 } } });
	try {
		expect(hooks.sourceStart(sub)).toBe(3);
		const subscribed = tb.producer.subscription.changed();
		front.serve(b.consume());
		await subscribed;
		expect(hooks.sourceStart(sub)).toBe(5);
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("a fork continues its open group across a route failure after its parent closes", async () => {
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	const front = new Front();
	front.serve(a.consume());
	const parent = front.consumer.track("seq").subscribe();
	const fork = parent.fork();
	try {
		const stalled = ta.write(0, 1, false);
		const group = await fork.recvGroup();
		if (!group) throw new Error("expected group 0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
		parent.close();

		const next = group.readFrame();
		tb.write(0, 3);
		front.serve(b.consume());
		stalled.abort(new StreamError(StreamCode.SessionClosed));
		expect(dec.decode((await next)?.payload)).toBe("0.1");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.2");
		expect(await group.readFrame()).toBeUndefined();
	} finally {
		parent.close();
		fork.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test.each(["conclude", "park"] as const)("a fork made after the front's %s replays its parent's copy", async (end) => {
	const a = new BroadcastProducer();
	const ta = copy(a);
	const front = new Front();
	front.serve(a.consume());
	const parent = front.consumer.track("seq").subscribe();
	ta.write(0);
	ta.write(1);
	expect(drain(parent)).toEqual([1]);
	front[end]();
	const fork = parent.fork({ maxDelay: Milli(10_000) });
	try {
		expect(drain(fork)).toEqual([0, 1]);
	} finally {
		parent.close();
		fork.close();
		front.consumer.close();
		a.close();
	}
});

test("a resumed fork has its own preferences and replay cursor", async () => {
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	const front = new Front();
	front.serve(a.consume());
	const parent = front.consumer.track("seq").subscribe({
		priority: 2,
		groups: { start: { included: 1 } },
	});
	ta.write(0);
	ta.write(1);
	expect(drain(parent)).toEqual([1]);
	const fork = parent.fork({ priority: 7, maxDelay: Milli(10_000) });
	try {
		expect(drain(fork)).toEqual([0, 1]);
		fork.update({ priority: 9 });
		expect(parent.subscription.peek()?.priority).toBe(2);
		expect(fork.subscription.peek()?.priority).toBe(9);
		const subscribed = tb.producer.subscription.changed();
		front.serve(b.consume());
		expect((await subscribed)?.priority).toBe(9);
		const narrowed = tb.producer.subscription.changed();
		fork.close();
		expect((await narrowed)?.priority).toBe(2);
		tb.write(2);
		expect(drain(parent)).toEqual([2]);
		parent.close();
		await unwanted(tb.producer.demand());
	} finally {
		parent.close();
		fork.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("a resumed fork shares an on-demand track request", async () => {
	const a = new BroadcastProducer();
	const requested = wireOf(a).requested();
	const front = new Front();
	front.serve(a.consume());
	const parent = front.consumer.track("seq").subscribe();
	const request = await requested;
	if (!request) throw new Error("expected on-demand track request");
	const producer = request.accept();
	const extra = wireOf(a).requested();
	const fork = parent.fork();
	try {
		producer.writeFrame({ payload: enc.encode("shared") });
		for (const subscriber of [parent, fork]) {
			const group = await subscriber.recvGroup();
			expect(dec.decode((await group?.readFrame())?.payload)).toBe("shared");
			group?.close();
		}
	} finally {
		parent.close();
		fork.close();
		front.consumer.close();
		a.close();
	}
	expect(await extra).toBeUndefined();
});

test("a lagging same-epoch replica resumes from one publisher's bytes exactly once", async () => {
	const { clock, installed } = manual();
	const publisher = new Producer();
	const originalPublication = publisher.createBroadcast(path);
	const original = originalPublication.broadcast;
	originalPublication.announce({ epoch: EPOCH });
	const source = original.createTrack("seq");
	const origin = new Producer();

	// Both replicas forward this one source. Only the incumbent pauses mid-group.
	function replica(lagging: boolean) {
		const broadcast = new BroadcastProducer();
		const output = broadcast.createTrack("seq");
		const input = source.subscribe({ maxDelay: Milli(60_000) });
		const release = Promise.withResolvers<void>();
		let stopped = false;
		const done = (async () => {
			try {
				for (;;) {
					const group = await input.recvGroup();
					if (!group) return;
					const copy = output.createGroup(group.sequence);
					try {
						for (let index = 0; ; index++) {
							const frame = await group.readFrame();
							if (!frame) break;
							if (lagging && group.sequence === 1 && index === 2) await release.promise;
							copy.writeFrame(frame);
						}
						copy.finish();
					} finally {
						group.close();
					}
				}
			} catch (err) {
				if (stopped) return;
				output.abort(err instanceof Error ? err : new Error(String(err)));
				broadcast.close();
				return err;
			}
		})();
		return {
			broadcast,
			done,
			stop(err?: Error) {
				stopped = true;
				input.close();
				release.resolve();
				if (err) output.abort(err);
				else {
					output.finish();
					output.close();
				}
				broadcast.close();
			},
		};
	}

	const a = replica(true);
	const b = replica(false);
	const disposeA = serve(origin, a.broadcast, 5n);
	const request = origin.request(path);
	const standby = b.broadcast.consume();
	const warm = standby.track("seq").subscribe({ maxDelay: Milli(60_000) });
	let sub: track.Subscriber | undefined;
	let disposeB: (() => void) | undefined;
	try {
		await clock.advance(Nano(0));
		const active = request.active.peek();
		if (!active) throw new Error("expected the incumbent route");
		sub = active.track("seq").subscribe({ maxDelay: Milli(60_000) });
		const first = source.appendGroup();
		for (let frame = 0; frame < 4; frame++) first.writeFrame({ payload: enc.encode(`0.${frame}`) });
		first.finish();
		const seen: string[] = [];
		const initial = await sub.recvGroup();
		if (!initial) throw new Error("expected group 0");
		expect(initial.sequence).toBe(0);
		for (let frame = 0; frame < 4; frame++) seen.push(dec.decode((await initial.readFrame())?.payload));
		expect(await initial.readFrame()).toBeUndefined();
		initial.close();

		const open = source.appendGroup();
		for (let frame = 0; frame < 2; frame++) open.writeFrame({ payload: enc.encode(`1.${frame}`) });
		const interrupted = await sub.recvGroup();
		if (!interrupted) throw new Error("expected group 1");
		expect(interrupted.sequence).toBe(1);
		for (let frame = 0; frame < 2; frame++) seen.push(dec.decode((await interrupted.readFrame())?.payload));
		for (let frame = 2; frame < 4; frame++) open.writeFrame({ payload: enc.encode(`1.${frame}`) });
		open.finish();
		const last = source.appendGroup();
		for (let frame = 0; frame < 4; frame++) last.writeFrame({ payload: enc.encode(`2.${frame}`) });
		last.finish();

		// The standby holds the complete original while the incumbent cannot pass 1.1.
		for (let sequence = 0; sequence < 3; sequence++) {
			const group = await warm.recvGroup();
			if (!group) throw new Error("expected a warmed group");
			expect(group.sequence).toBe(sequence);
			for (let frame = 0; frame < 4; frame++) {
				expect(dec.decode((await group.readFrame())?.payload)).toBe(`${sequence}.${frame}`);
			}
			expect(await group.readFrame()).toBeUndefined();
			group.close();
		}
		disposeB = serve(origin, b.broadcast, 9n);
		await clock.advance(Nano(0));
		expect(request.active.peek()).toBe(active);
		disposeA();
		a.stop(new StreamError(StreamCode.SessionClosed));
		await clock.advance(Nano(0));
		for (let frame = 2; frame < 4; frame++) seen.push(dec.decode((await interrupted.readFrame())?.payload));
		expect(await interrupted.readFrame()).toBeUndefined();
		interrupted.close();
		const successor = await sub.recvGroup();
		if (!successor) throw new Error("expected group 2");
		expect(successor.sequence).toBe(2);
		for (let frame = 0; frame < 4; frame++) seen.push(dec.decode((await successor.readFrame())?.payload));
		expect(await successor.readFrame()).toBeUndefined();
		successor.close();
		expect(seen).toEqual(Array.from({ length: 12 }, (_, index) => `${Math.floor(index / 4)}.${index % 4}`));
		await clock.advance(Nano(0));
		expect(sub.tryRecvGroup()).toBeUndefined();
		// The source is still open, so the resumed subscription must stay live.
		expect(sub.closed.peek()).toBeUndefined();
	} finally {
		sub?.close();
		warm.close();
		standby.close();
		request.close();
		disposeA();
		disposeB?.();
		a.stop();
		b.stop();
		original.close();
		publisher.close();
		origin.close();
		const outcomes = await Promise.all([a.done, b.done]);
		installed.close();
		expect(outcomes).toEqual([undefined, undefined]);
	}
});

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
	stalled.abort(new StreamError(StreamCode.SessionClosed));
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
	stalled.abort(new StreamError(StreamCode.SessionClosed));
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
	old.abort(new StreamError(StreamCode.SessionClosed));
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
	old.abort(new StreamError(StreamCode.SessionClosed));
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
		const failing = a.createTrack("fetch").createGroup(5);
		const group = await front.consumer.track("fetch").fetchGroup(5);
		const read = group.readFrame();
		const whole = b.createTrack("fetch").createGroup(5);
		whole.finish();
		front.serve(b.consume());
		await settle();
		failing.abort(new StreamError(StreamCode.SessionClosed));
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
		const failing = fa.createGroup(5);
		failing.writeFrame({ payload: enc.encode("5.0") });
		const fetching = front.consumer.track("fetch").fetchGroup(5);
		for (let turn = 0; turn < turns; turn++) await Promise.resolve();
		failing.abort(new StreamError(StreamCode.SessionClosed));
		await settle();

		// The next route holds the fetched group whole; the old one still feeds the reader a
		// group the new route never has.
		const fb = b.createTrack("fetch");
		const whole = fb.createGroup(5);
		whole.writeFrame({ payload: enc.encode("5.0") });
		whole.writeFrame({ payload: enc.encode("5.1") });
		whole.finish();
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

test("a replacement delivers unseen datagrams below the newest sequence", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const { a, b, swap, active, close } = await setup();
		const ta = copy(a);
		const tb = copy(b);
		const sub = active.track("seq").subscribe({ maxDelay: Milli(100) });
		ta.producer.insertDatagram(10, undefined, enc.encode("10"));
		expect((await sub.recvDatagram())?.sequence).toBe(10);
		await swap();
		tb.producer.insertDatagram(10, undefined, enc.encode("10"));
		tb.producer.insertDatagram(5, undefined, enc.encode("5"));
		const next = hooks.tryRecvDatagram(sub);
		expect(next.kind).toBe("datagram");
		if (next.kind === "datagram") expect(next.datagram.sequence).toBe(5);
		expect(hooks.tryRecvDatagram(sub).kind).toBe("idle");
		sub.close();
		close();
	} finally {
		installed.close();
	}
});

test.each([0, 100])("datagram identities stay within the reader's %p ms budget", async (budget) => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const front = new Front();
		const a = new BroadcastProducer();
		const ta = copy(a);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(budget) });
		const send = (sequence: number) => ta.producer.insertDatagram(sequence, undefined, enc.encode("d"));
		const recv = () => hooks.tryRecvDatagram(sub);
		send(10);
		expect(recv().kind).toBe("datagram");
		await clock.advance(Nano(60_000_000_000));
		send(10);
		expect(recv().kind).toBe("idle");
		send(5);
		expect(recv().kind).toBe("datagram");
		if (budget > 0) {
			await clock.advance(Nano(budget * 500_000));
			send(10);
			expect(recv().kind).toBe("idle");
			await clock.advance(Nano(budget * 500_000));
		}
		send(10);
		expect(recv().kind).toBe("datagram");
		sub.close();
		front.consumer.close();
		a.close();
	} finally {
		installed.close();
	}
});

test("datagram identities are capped by the replay buffer", () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	// Superseded identities never expire within an hour, so only the cap bounds them.
	const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) });
	const send = (sequence: number) => ta.producer.insertDatagram(sequence, undefined, enc.encode("d"));
	const recv = () => hooks.tryRecvDatagram(sub).kind;
	for (let sequence = 0; sequence <= MAX_DATAGRAMS + 1; sequence++) {
		send(sequence);
		expect(recv()).toBe("datagram");
	}
	send(1);
	expect(recv()).toBe("idle");
	send(0);
	expect(recv()).toBe("datagram");
	sub.close();
	front.consumer.close();
	a.close();
});

test("a route that goes for good ends the track as its last copy does", async () => {
	const { a, active, disposeA, request, close } = await setup();
	const ta = copy(a);
	const sub = active.track("seq").subscribe();
	ta.write(0);
	expect(drain(sub)).toEqual([0]);

	ta.producer.abort(new StreamError(StreamCode.SessionClosed));
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
	hooks.acceptRequest(pending, b.consume());
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
	stalled.abort(new StreamError(StreamCode.SessionClosed));
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

	old.abort(new StreamError(StreamCode.SessionClosed));
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
	stalled.abort(new StreamError(StreamCode.SessionClosed));
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

// With the subscription still open, no successor, no later writes, and the fetch for the rest
// still unanswered: whether the old route failed or only stalled.
for (const fails of [true, false]) {
	test(`a resumed group ends with the serving route's drop${fails ? " once its old route failed" : ""}`, async () => {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		const stalled = ta.write(0, 1, false);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(60_000) });
		const group = sub.tryRecvGroup();
		if (!group) throw new Error("expected group 0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
		if (fails) stalled.abort(new StreamError(StreamCode.SessionClosed));

		front.serve(b.consume());
		const read = group.readFrame();
		await settle();
		expect(await isPending(read)).toBe(true);
		expect(tb.producer.demand().wanted.peek()).not.toBeUndefined();

		const cut = new StreamError(StreamCode(70), { message: "cut" });
		hooks.dropTrackGroups(tb.producer, 0, 1, cut);
		await expect(read).rejects.toBe(cut);

		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	});
}

/** What a resumed read with dispositions found: a group's sequence, or a drop's run and code. */
function recvDisposition(sub: track.Subscriber): number | [number, number, number] | string {
	const recv = hooks.tryRecvGroup(sub, true);
	if (recv.kind === "group") {
		recv.group.close();
		return recv.group.sequence;
	}
	if (recv.kind === "drop") return [recv.start, recv.end, (recv.error as StreamError).code];
	return recv.kind;
}

test("a resumed reader reports a drop once and still serves a late group", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(60_000) });
	ta.write(0);
	expect(recvDisposition(sub)).toBe(0);
	// A route failing is no verdict: another route may still deliver the group.
	hooks.dropTrackGroups(ta.producer, 1, 2, new StreamError(StreamCode.SessionClosed));
	expect(recvDisposition(sub)).toBe("idle");
	hooks.dropTrackGroups(ta.producer, 2, 4, new StreamError(StreamCode(70), { message: "cut" }));
	expect(recvDisposition(sub)).toEqual([2, 4, 70]);
	expect(recvDisposition(sub)).toBe("idle");
	// Re-created under a run already reported dropped.
	ta.write(3);
	ta.write(4);
	expect(recvDisposition(sub)).toBe(3);
	expect(recvDisposition(sub)).toBe(4);
	expect(recvDisposition(sub)).toBe("idle");

	sub.close();
	front.consumer.close();
	a.close();
});

test("a separately polled drop does not deliver its run", () => {
	const front = new Front();
	const source = new BroadcastProducer();
	const track = copy(source);
	front.serve(source.consume());
	const sub = front.consumer.track("seq").subscribe();
	track.write(0);
	expect(drain(sub)).toEqual([0]);
	const error = new StreamError(StreamCode.NotFound);
	hooks.dropTrackGroups(track.producer, 2, 4, error);
	expect(hooks.tryDrop(sub)).toEqual({ kind: "drop", start: 2, end: 4, error });
	expect(hooks.tryDrop(sub)).toBeUndefined();
	track.write(3);
	expect(drain(sub)).toEqual([3]);
	expect(hooks.tryDrop(sub)).toBeUndefined();
	expect(drain(sub)).toEqual([]);
	sub.close();
	front.consumer.close();
	source.close();
});

test("a resumed reader reports a group its serving copy skipped past the budget", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const front = new Front();
		const a = new BroadcastProducer();
		const ta = copy(a);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(1_000) });
		expect(recvDisposition(sub)).toBe("idle");
		ta.write(0);
		ta.write(1);
		await clock.advance(Nano(2_000_000_000));
		expect(recvDisposition(sub)).toBe(1);
		expect(recvDisposition(sub)).toEqual([0, 1, StreamCode.Old]);
		expect(recvDisposition(sub)).toBe("idle");

		sub.close();
		front.consumer.close();
		a.close();
	} finally {
		installed.close();
	}
});

test("a drop waits while a copy the serving one replaced may still deliver it", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(1_000) });
		ta.write(0);
		expect(recvDisposition(sub)).toBe(0);
		front.serve(b.consume());
		await settle();
		// Only the new route gave groups 1 and 2 up; the old one is not yet bounded.
		hooks.dropTrackGroups(tb.producer, 1, 3, new StreamError(StreamCode(70), { message: "cut" }));
		expect(recvDisposition(sub)).toBe("idle");
		ta.write(1);
		expect(recvDisposition(sub)).toBe(1);
		// The new route's first group bounds the old one just below it, which may still get 2.
		tb.write(3);
		expect(recvDisposition(sub)).toBe(3);
		expect(recvDisposition(sub)).toBe("idle");
		// Once the old copy is past the reader's budget, nothing delivers 2.
		await clock.advance(Nano(1_000_000_000));
		expect(recvDisposition(sub)).toEqual([2, 3, 70]);
		ta.write(2);
		expect(recvDisposition(sub)).toBe("idle");

		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	} finally {
		installed.close();
	}
});

test("a drop a replaced route held back is no verdict on the route serving next", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const c = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	const tc = copy(c);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	expect(recvDisposition(sub)).toBe(0);
	front.serve(b.consume());
	await settle();
	// Held back: the copy b replaced may still deliver group 1.
	hooks.dropTrackGroups(tb.producer, 1, 2, new StreamError(StreamCode(70), { message: "cut" }));
	expect(recvDisposition(sub)).toBe("idle");
	front.serve(c.consume());
	await settle();
	ta.producer.abort(new StreamError(StreamCode.SessionClosed));
	tb.producer.abort(new StreamError(StreamCode.SessionClosed));
	await settle();
	expect(recvDisposition(sub)).toBe("idle");
	tc.write(1);
	expect(recvDisposition(sub)).toBe(1);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
	c.close();
});

test("a front serving another front reports the inner serving route's drop", async () => {
	const inner = new Front();
	const outer = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	inner.serve(a.consume());
	outer.serve(inner.consumer);
	const sub = outer.consumer.track("seq").subscribe({ maxDelay: Milli(60_000) });
	ta.write(0);
	expect(recvDisposition(sub)).toBe(0);
	hooks.dropTrackGroups(ta.producer, 1, 3, new StreamError(StreamCode(70), { message: "cut" }));
	expect(recvDisposition(sub)).toEqual([1, 3, 70]);
	expect(recvDisposition(sub)).toBe("idle");
	// Re-created under a run already reported dropped.
	ta.write(2);
	ta.write(3);
	expect(recvDisposition(sub)).toBe(2);
	expect(recvDisposition(sub)).toBe(3);
	expect(recvDisposition(sub)).toBe("idle");

	sub.close();
	outer.consumer.close();
	inner.consumer.close();
	a.close();
});

for (const [field, first, second] of [
	["priority", { priority: 1 }, { priority: 2 }],
	["compression", { compress: true }, { compress: false }],
	["refresh", { refresh: 4 }, {}],
] as const) {
	test(`track info keeps one ${field} answer across routes`, async () => {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		a.createTrack("seq", first);
		b.createTrack("seq", second);
		front.serve(a.consume());
		expect(await front.consumer.track("seq").info()).toMatchObject(first);
		front.serve(b.consume());
		await expect(front.consumer.track("seq").info()).rejects.toThrow("changed its properties");
		front.consumer.close();
		a.close();
		b.close();
	});
}

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
	const stalled = ta.createGroup(0);
	stalled.writeFrame({ payload: enc.encode("0.0") });
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	await group.readFrame();
	await sub.info();

	front.serve(b.consume());
	await settle();
	stalled.abort(new StreamError(StreamCode.SessionClosed));
	await expect(group.readFrame()).rejects.toThrow("changed its properties");

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

/** A route offering no FETCH, like a moq-transport session's, through every handle the front clones. */
class Unfetchable extends BroadcastConsumer {
	constructor(shared?: never) {
		super(shared);
		overrideBroadcastWire(this, { fetchGroup: undefined });
	}

	override clone(): Unfetchable {
		return new Unfetchable(this.shareState());
	}

	static of(consumer: BroadcastConsumer): Unfetchable {
		return new Unfetchable((consumer as Unfetchable).shareState());
	}
}

test("a failed group whose replacement cannot fetch ends with the failure, or continues from its copy", async () => {
	for (const held of [false, true]) {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		front.serve(a.consume());
		// A budget wide enough that the newer group never gives group 0 up first.
		const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) });
		const stalled = ta.write(0, 1, false);
		const group = sub.tryRecvGroup();
		if (!group) throw new Error("expected group 0");
		await group.readFrame();

		// Like a moq-transport session: the route offers no FETCH to fill the rest from.
		if (held) tb.write(0, 2);
		else tb.write(1);
		front.serve(Unfetchable.of(b.consume()));
		await settle();
		const failure = new StreamError(StreamCode.Cancel);
		stalled.abort(failure);
		if (held) {
			expect(dec.decode((await group.readFrame())?.payload)).toBe("0.1");
			expect(await group.readFrame()).toBeUndefined();
		} else {
			await expect(group.readFrame()).rejects.toBe(failure);
		}

		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("draining groups to their end leaves buffered datagrams readable", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	ta.producer.appendDatagram(undefined, enc.encode("d"));
	ta.producer.finish();
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
	const group = tb.createGroup(0);
	group.writeFrame({ payload: enc.encode("b") });
	group.finish();
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
		const group = producer.createGroup(0);
		group.writeFrame({ payload: enc.encode(`${priority}`) });
		group.finish();
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
		const fail = () => failing.abort(new StreamError(StreamCode.SessionClosed));
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
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		const later = next();
		await settle();
		failing.abort(new StreamError(StreamCode.SessionClosed));
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
	failing.abort(new StreamError(StreamCode.SessionClosed));
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
	failed.abort(new StreamError(StreamCode.SessionClosed));
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
	const pulled = wireOf(b).requested();
	front.serve(b.consume());
	await settle();
	const request = await pulled;
	if (!request) throw new Error("b was never asked");

	front.serve(c.consume());
	await settle();
	// Nobody asks b any more, so its late answer is let go.
	const late = request.accept();
	await settle();
	expect(late.closed.peek()).toBeDefined();

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

test("a serving track verdict keeps its cause after delivering", async () => {
	const front = new Front();
	const source = new BroadcastProducer();
	const track = copy(source);
	front.serve(source.consume());
	const sub = front.consumer.track("seq").subscribe();
	try {
		track.write(0);
		const first = await sub.recvGroup();
		expect(first?.sequence).toBe(0);
		first?.close();
		const cause = new StreamError(StreamCode.FrameTooLarge);
		track.producer.abort(cause);
		await expect(sub.recvGroup()).rejects.toBe(cause);
	} finally {
		sub.close();
		front.consumer.close();
		source.close();
	}
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
	const group = tb.createGroup(0);
	group.writeFrame({ payload: enc.encode("b") });
	group.finish();
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

// A front's reader reports the serving copy's largest only within its bounds: the cap it sets
// and the floor its subscription names.
test("the largest honours the reader's bounds on the serving copy", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const capped = front.consumer
		.track("seq")
		.subscribe()
		.withRange({ end: { excluded: 3 } });
	const floored = front.consumer.track("seq").subscribe({ groups: { start: { included: 8 } } });
	for (let sequence = 0; sequence < 5; sequence++) ta.write(sequence);

	const next = capped.largest();
	expect(await isPending(next)).toBe(true);
	capped.withRange({});
	expect(await next).toEqual({ group: 4, frame: 0 });

	const floor = floored.largest();
	expect(await isPending(floor)).toBe(true);
	ta.write(8);
	expect(await floor).toEqual({ group: 8, frame: 0 });

	capped.close();
	floored.close();
	front.consumer.close();
	a.close();
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
	ta.producer.abort(new StreamError(StreamCode.SessionClosed));
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
	tb.producer.finish();
	tb.producer.close();
	expect(await last).toBeUndefined();

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

/** A manual clock, installed until `installed` closes. */
function manual() {
	const clock = new Manual();
	return { clock, installed: clock.install() };
}

const SECOND = Milli(1000);

test.each(["lands", "dropped", "expired"])(
	"an arrival reader gets a group still in flight on the replaced route (%s)",
	async (outcome) => {
		const { clock, installed } = manual();
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ maxDelay: SECOND });
		try {
			ta.write(0);
			expect(drain(sub)).toEqual([0]);
			front.serve(b.consume());
			await settle();
			tb.write(2);
			expect(drain(sub)).toEqual([2]);
			expect(drain(sub)).toEqual([]);
			expect(ta.producer.demand().wanted.peek()).toBeDefined();

			if (outcome === "dropped") hooks.dropTrackGroups(ta.producer, 1, 2, new NotFound("group"));
			if (outcome === "expired") await clock.advance(Nano.fromMilli(SECOND));
			if (outcome !== "lands") {
				// The groups drained above are released once their close settles.
				await settle();
				expect(drain(sub)).toEqual([]);
				await settle();
				expect(ta.producer.demand().wanted.peek()).toBeUndefined();
			}
			ta.write(1);
			expect(drain(sub)).toEqual(outcome === "lands" ? [1] : []);
		} finally {
			sub.close();
			front.consumer.close();
			a.close();
			b.close();
			installed.close();
		}
	},
);

test("a group read whole still expires once the reader lets go of its copy", async () => {
	const { clock, installed } = manual();
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ maxDelay: SECOND });
	try {
		ta.write(0);
		const group = sub.tryRecvGroup();
		if (!group) throw new Error("expected group 0");
		// Read whole and released, as a publisher does before it drains the stream it wrote it to.
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
		expect(await group.readFrame()).toBeUndefined();
		group.close();

		front.serve(b.consume());
		await settle();
		tb.write(2);
		expect(drain(sub)).toEqual([2]);
		// Its successor lands on the replaced copy, starting the budget there.
		const half = Nano.fromMilli(Milli(SECOND / 2));
		await clock.advance(half);
		ta.write(1);
		drain(sub);

		let expired: Error | undefined;
		void group.expired.then((err) => {
			expired = err;
		});
		// A budget after the swap, the next read lets go of the replaced copy, closing its subscription.
		await clock.advance(half);
		expect(drain(sub)).toEqual([]);
		await settle();
		expect(ta.producer.demand().wanted.peek()).toBeUndefined();
		expect(expired).toBeUndefined();

		await clock.advance(half);
		await settle();
		expect(expired).toBeInstanceOf(StreamError);
		expect((expired as StreamError).code).toBe(StreamCode.Old);
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
		installed.close();
	}
});

test("an ordered reader keeps no copy for a group it passed", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ maxDelay: SECOND });
	const next = () => {
		const recv = hooks.tryNextGroup(sub);
		if (recv.kind !== "group") return undefined;
		recv.group.close();
		return recv.group.sequence;
	};
	ta.write(0);
	expect(next()).toBe(0);
	front.serve(b.consume());
	await settle();
	tb.write(2);
	expect(next()).toBe(2);
	// Group 2 is released once its close settles.
	await settle();
	expect(next()).toBeUndefined();
	await settle();
	expect(ta.producer.demand().wanted.peek()).toBeUndefined();

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test.each([true, false])(
	"a group the replaced copy gets late backs the serving copy's (ordered: %p)",
	async (ordered) => {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) });
		const view = ordered ? sub.ordered() : undefined;
		const next = () => (view ? view.nextGroup() : sub.recvGroup());
		ta.write(0);
		(await next())?.close();
		front.serve(b.consume());
		await settle();

		const failing = tb.write(1, 1, false);
		const group = await next();
		if (group?.sequence !== 1) throw new Error("expected group 1");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.0");
		// The reader reads on before the old route has group 1 at all.
		const later = next();
		await settle();

		// The old route finishes group 1 only now, and the new route's copy of it fails.
		ta.write(1, 2);
		failing.abort(new StreamError(StreamCode.SessionClosed));
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

test.each([true, false])("a replaced copy backs a group before its first arrival (ordered: %p)", async (ordered) => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const c = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	const tc = copy(c);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) });
	const view = ordered ? sub.ordered() : undefined;
	const next = () => (view ? view.nextGroup() : sub.recvGroup());
	ta.write(0);
	(await next())?.close();
	front.serve(b.consume());
	await settle();
	// Its route fails once replaced: closing cleanly would declare the track ends at group 1.
	ta.producer.abort(new StreamError(StreamCode.SessionClosed));
	front.serve(c.consume());
	await settle();

	const failing = tc.write(1, 1, false);
	const group = await next();
	if (group?.sequence !== 1) throw new Error("expected group 1");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("1.0");
	const later = next();
	await settle();

	// b's first group lands only now, and c's copy of it fails.
	tb.write(1, 2);
	failing.abort(new StreamError(StreamCode.SessionClosed));
	expect(dec.decode((await group.readFrame())?.payload)).toBe("1.1");
	expect(await group.readFrame()).toBeUndefined();

	group.close();
	tc.write(2);
	expect((await later)?.sequence).toBe(2);
	view?.close();
	sub.close();
	front.consumer.close();
	a.close();
	b.close();
	c.close();
});

test("a clean end waits for a group the replaced copy still carries", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ maxDelay: SECOND });
	try {
		ta.write(0);
		ta.write(2);
		expect(drain(sub)).toEqual([0, 2]);

		// The new route has only the group already handed out, and ends there.
		tb.write(2);
		tb.producer.finish();
		tb.producer.close();
		front.serve(b.consume());
		await settle();
		const next = sub.recvGroup();
		expect(await isPending(next)).toBe(true);

		ta.write(1);
		expect((await next)?.sequence).toBe(1);
		expect(await sub.recvGroup()).toBeUndefined();
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test.each([false, true])(
	"a reader that asked for groups keeps a replaced copy's unread group across a datagram read (datagrams first: %p)",
	async (datagramsFirst) => {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		copy(b);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(10_000) });
		expect(hooks.tryRecvGroup(sub, false).kind).toBe("idle");
		expect(hooks.tryRecvDatagram(sub).kind).toBe("idle");
		ta.write(0);
		front.serve(b.consume());
		await settle();
		if (datagramsFirst) expect(hooks.tryRecvDatagram(sub).kind).toBe("idle");
		expect(drain(sub)).toEqual([0]);

		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	},
);

test.each([Milli(100), Milli(3_600_000)])(
	"a datagram reader keeps no replaced copy, whatever its budget (%p ms)",
	async (maxDelay) => {
		const front = new Front();
		const sources: BroadcastProducer[] = [];
		const copies: ReturnType<typeof copy>[] = [];
		const sub = front.consumer.track("seq").subscribe({ maxDelay });
		try {
			for (let sequence = 0; sequence < 20; sequence++) {
				const source = new BroadcastProducer();
				const held = copy(source);
				sources.push(source);
				copies.push(held);
				front.serve(source.consume());
				await settle();
				held.write(sequence);
				expect(hooks.tryRecvDatagram(sub).kind).toBe("idle");
				await settle();
				const subscribed = copies.filter((held) => held.producer.demand().wanted.peek() !== undefined);
				expect(subscribed).toEqual([held]);
			}
			// Only the serving copy's group is left.
			expect(drain(sub)).toEqual([19]);
		} finally {
			sub.close();
			front.consumer.close();
			for (const source of sources) source.close();
		}
	},
);

test("a group-only reader of a finite track settles closed and lets go of its copy", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	// Declared but still open, so the copy's subscription is what holds the demand.
	ta.producer.finishAt({ group: 1, frame: 0 });
	expect(drain(sub)).toEqual([0]);
	expect(await sub.recvGroup()).toBeUndefined();
	await settle();
	expect(sub.closed.peek()).toBeNull();
	expect(ta.producer.demand().wanted.peek()).toBeUndefined();

	sub.close();
	front.consumer.close();
	a.close();
});

test("a routed track's errored end outlives the copy that declared it", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	const failed = new StreamError(StreamCode(71));
	ta.producer.finishAt({ group: 1, frame: 0 }, failed);
	const group = await sub.recvGroup();
	expect(group?.sequence).toBe(0);
	group?.close();
	// Concluding releases the copy that declared the end.
	await settle();
	await expect(sub.recvGroup()).rejects.toBe(failed);

	sub.close();
	front.consumer.close();
	a.close();
});

test("a group-only fork of a finite track settles closed and lets go of its copy", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const parent = front.consumer.track("seq").subscribe();
	const fork = parent.fork();
	parent.close();
	ta.write(0);
	ta.producer.finishAt({ group: 1, frame: 0 });
	expect(drain(fork)).toEqual([0]);
	expect(await fork.recvGroup()).toBeUndefined();
	await settle();
	expect(fork.closed.peek()).toBeNull();
	expect(ta.producer.demand().wanted.peek()).toBeUndefined();

	fork.close();
	front.consumer.close();
	a.close();
});

test("the serving copy closing cleanly settles closed and leaves its datagrams readable", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	ta.producer.appendDatagram(undefined, enc.encode("d"));
	ta.producer.finish();
	ta.producer.close();
	await settle();
	expect(sub.closed.peek()).toBeNull();
	expect(drain(sub)).toEqual([0]);
	expect(dec.decode((await sub.recvDatagram())?.payload)).toBe("d");
	expect(await sub.recvDatagram()).toBeUndefined();
	expect(await sub.recvGroup()).toBeUndefined();

	sub.close();
	front.consumer.close();
	a.close();
});

test("an end a replaced copy declared holds across a swap", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	ta.producer.finishAt({ group: 2, frame: 0 });
	await settle();

	// b serves before its subscription learns the end.
	front.serve(b.consume());
	await settle();
	expect(hooks.subscriberEnd(sub)).toEqual({ group: 2, frame: 0 });
	expect(await sub.finished()).toEqual({ group: 2, frame: 0 });

	// A copy declaring another end claims different content.
	tb.producer.finishAt({ group: 3, frame: 0 });
	await settle();
	expect(sub.closed.peek()).toBeInstanceOf(Error);
	expect(hooks.subscriberEnd(sub)).toEqual({ group: 2, frame: 0 });
	// Refused, the reader holds no copy.
	expect(ta.producer.demand().wanted.peek()).toBeUndefined();
	expect(tb.producer.demand().wanted.peek()).toBeUndefined();

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("an end a replaced copy declared holds once the copy is let go in the same turn", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	front.serve(b.consume());
	await settle();

	// A datagram read lets go of the replaced copy before its end is reported.
	ta.producer.finishAt({ group: 2, frame: 0 });
	expect(hooks.tryRecvDatagram(sub).kind).toBe("idle");
	expect(hooks.subscriberEnd(sub)).toEqual({ group: 2, frame: 0 });

	tb.producer.finishAt({ group: 3, frame: 0 });
	await settle();
	expect(sub.closed.peek()).toBeInstanceOf(Error);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a replaced copy whose session closed binds no end", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	await settle();
	ta.write(0);
	await settle();
	expect(drain(sub)).toEqual([0]);

	front.serve(b.consume());
	await settle();
	// A session close, local or remote, cancels the copy: a failed route, not an end at group 1.
	ta.producer.abort(new SessionError(SessionCode.Cancel));
	tb.write(1);
	await settle();
	expect(drain(sub)).toEqual([1]);
	expect(hooks.subscriberEnd(sub)).toBeUndefined();
	expect(sub.closed.peek()).toBeUndefined();

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("largest ends once the serving copy reaches its end, with the front still open", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0);
	ta.producer.finishAt({ group: 1, frame: 0 });
	expect(await sub.largest()).toEqual({ group: 0, frame: 0 });
	const next = sub.largest();
	expect(await isPending(next)).toBe(false);
	expect(await next).toBeUndefined();

	sub.close();
	front.consumer.close();
	a.close();
});

test("closing with an abort rejects a pending frame read of a group handed out", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0, 1, false);
	const group = await sub.recvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
	const read = group.readFrame();
	const abort = new Error("aborted");
	sub.close(abort);
	await expect(read).rejects.toBe(abort);

	front.consumer.close();
	a.close();
});

test("closing with an abort settles a recovery in flight with it", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	const stalled = ta.write(0, 1, false);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");

	// The next route does not have the group yet: recovery fetches it there and waits.
	front.serve(b.consume());
	await settle();
	stalled.abort(new StreamError(StreamCode.SessionClosed));
	const read = group.readFrame();
	await settle();
	const abort = new Error("aborted");
	sub.close(abort);
	// A replacement arriving afterwards is not adopted.
	tb.write(0, 2);
	await expect(read).rejects.toBe(abort);

	front.consumer.close();
	a.close();
	b.close();
});

test.each(["lands", "expired"])(
	"the serving copy closing cleanly settles closed once a group the replaced copy may carry is in (%s)",
	async (outcome) => {
		const { clock, installed } = manual();
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ maxDelay: SECOND });
		try {
			ta.write(0);
			ta.write(2);
			expect(drain(sub)).toEqual([0, 2]);

			// The new route has only the group already handed out, and ends there. Nothing reads.
			tb.write(2);
			tb.producer.finish();
			tb.producer.close();
			front.serve(b.consume());
			await settle();
			expect(sub.closed.peek()).toBeUndefined();

			if (outcome === "lands") ta.write(1);
			else await clock.advance(Nano.fromMilli(SECOND));
			await settle();
			// Settled like a direct reader's, with the group that landed still readable.
			expect(sub.closed.peek()).toBeNull();
			expect(drain(sub)).toEqual(outcome === "lands" ? [1] : []);
			expect(await sub.recvGroup()).toBeUndefined();
			await settle();
			expect(ta.producer.demand().wanted.peek()).toBeUndefined();
		} finally {
			sub.close();
			front.consumer.close();
			a.close();
			b.close();
			installed.close();
		}
	},
);

// Lowering a resumed reader's floor offers the groups below the old floor that only a copy it
// already let go cached: the serving route is asked for just those, whether it caches them or
// gets them once asked. An ordered reader never goes back, so it asks for nothing.
test.each([
	[true, false],
	[false, false],
	[true, true],
])(
	"a lowered floor asks the serving route for a released copy's groups (cached=%p, ordered=%p)",
	async (cached, ordered) => {
		const clock = new Manual();
		const installed = clock.install();
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) });
		const inOrder = ordered ? sub.ordered() : undefined;
		const read = () => {
			const sequences: number[] = [];
			const next = () => (inOrder ? inOrder.tryNextGroup() : sub.tryRecvGroup());
			for (let group = next(); group; group = next()) {
				sequences.push(group.sequence);
				group.close();
			}
			return sequences;
		};
		const asked = () => tb.producer.subscription.peek()?.groups?.start?.included;
		try {
			sub.withRange({ start: { included: 4 } });
			await clock.advance(Nano.zero);
			for (let sequence = 0; sequence < 6; sequence++) ta.write(sequence);
			expect(read()).toEqual([4, 5]);

			if (cached) for (let sequence = 0; sequence < 6; sequence++) tb.write(sequence);
			front.serve(b.consume());
			await clock.advance(Nano.zero);
			tb.write(6);
			expect(read()).toEqual([6]);
			await clock.advance(Nano.zero);
			expect(ta.producer.demand().wanted.peek()).toBeUndefined();
			expect(asked()).toBe(5);

			sub.withRange({ start: { included: 1 } });
			await clock.advance(Nano.zero);
			expect(asked()).toBe(ordered ? 5 : 1);
			if (!cached) {
				expect(read()).toEqual([]);
				for (let sequence = 1; sequence < 4; sequence++) tb.write(sequence);
			}
			expect(read()).toEqual(ordered ? [] : [1, 2, 3]);
			tb.write(7);
			expect(read()).toEqual([7]);
		} finally {
			sub.close();
			front.consumer.close();
			a.close();
			b.close();
			installed.close();
		}
	},
);

test("a datagram below the group floor does not keep a finite track's copy", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.producer.appendDatagram(undefined, enc.encode("d"));
	await settle();
	// The floor passes the datagram: a datagram read would only discard it.
	sub.withRange({ start: { included: 1 } });
	ta.write(1);
	ta.producer.finishAt({ group: 2, frame: 0 });
	expect(drain(sub)).toEqual([1]);
	expect(await sub.recvGroup()).toBeUndefined();
	await settle();
	expect(sub.closed.peek()).toBeNull();
	expect(ta.producer.demand().wanted.peek()).toBeUndefined();

	sub.close();
	front.consumer.close();
	a.close();
});

test("a clean end keeps the serving copy while a group read from a replaced one may continue from it", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) });
	try {
		ta.write(0);
		expect(drain(sub)).toEqual([0]);
		front.serve(b.consume());
		await settle();

		// Group 1 is handed out from the old route, its first frame still unread.
		const stalled = ta.write(1, 1, false);
		const group = await sub.recvGroup();
		if (group?.sequence !== 1) throw new Error("expected group 1");

		// The new route has all of group 1 and ends there.
		tb.write(1, 2);
		tb.producer.finish();
		tb.producer.close();
		await settle();
		expect(sub.closed.peek()).toBeNull();
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.0");

		// The old route fails: the group continues from the copy that ended.
		stalled.abort(new StreamError(StreamCode.SessionClosed));
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.1");
		expect(await group.readFrame()).toBeUndefined();
		group.close();
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("a retained copy declaring another end after a clean end fails the reads", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ maxDelay: SECOND });
	try {
		ta.write(0);
		ta.write(2);
		expect(drain(sub)).toEqual([0, 2]);

		// The new route ends at 3, while the old one still owes group 1.
		tb.write(2);
		tb.producer.finishAt({ group: 3, frame: 0 });
		tb.producer.finish();
		tb.producer.close();
		front.serve(b.consume());
		await settle();
		ta.write(1);
		await settle();
		expect(sub.closed.peek()).toBeNull();

		// The copy still holding group 1 claims another end: its content is refused.
		ta.producer.finishAt({ group: 5, frame: 0 });
		await settle();
		expect(hooks.subscriberEnd(sub)).toEqual({ group: 3, frame: 0 });
		await expect(sub.recvGroup()).rejects.toThrow("changed its properties");
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("a clean end keeps the serving copy while a group handed out before the swap may continue from it", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ maxDelay: Milli(3_600_000) });
	try {
		// Group 0 is handed out while the old route is the only one.
		const stalled = ta.write(0, 1, false);
		const group = await sub.recvGroup();
		if (group?.sequence !== 0) throw new Error("expected group 0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");

		// The new route has all of group 0 and ends there.
		front.serve(b.consume());
		await settle();
		tb.write(0, 2);
		tb.producer.finish();
		tb.producer.close();
		await settle();
		expect(sub.closed.peek()).toBeNull();

		// The old route fails: the group continues from the copy that ended.
		stalled.abort(new StreamError(StreamCode.SessionClosed));
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.1");
		expect(await group.readFrame()).toBeUndefined();
		group.close();
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test.each(["handed out", "buffered"])("an end below a group another copy %s is refused", async (seen) => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	try {
		ta.write(5);
		if (seen === "handed out") expect(drain(sub)).toEqual([5]);
		front.serve(b.consume());
		await settle();

		// The new route ends the track before a group the old one has.
		tb.producer.finishAt({ group: 3, frame: 0 });
		await settle();
		expect(() => sub.tryRecvGroup()).toThrow("changed its properties");
		expect(sub.closed.peek()).toBeInstanceOf(Error);
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test.each(["handed out", "buffered"])("an end below a datagram another copy %s is refused", async (seen) => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	try {
		ta.producer.insertDatagram(5, undefined, enc.encode("5"));
		if (seen === "handed out") expect(hooks.tryRecvDatagram(sub).kind).toBe("datagram");
		front.serve(b.consume());
		await settle();
		// The new route delivers an older datagram last, then ends the track before the old route's.
		tb.producer.insertDatagram(2, undefined, enc.encode("2"));
		if (seen === "handed out") expect(hooks.tryRecvDatagram(sub).kind).toBe("datagram");
		tb.producer.finishAt({ group: 3, frame: 0 });
		await settle();
		expect(hooks.tryRecvDatagram(sub).kind).toBe("error");
		expect(sub.closed.peek()).toBeInstanceOf(Error);
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("a group past the declared end on another copy is refused", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	try {
		ta.write(0);
		ta.producer.finishAt({ group: 2, frame: 0 });
		expect(drain(sub)).toEqual([0]);
		front.serve(b.consume());
		await settle();

		// The new route has a group the end says never exists.
		tb.write(3);
		await settle();
		expect(() => sub.tryRecvGroup()).toThrow("changed its properties");
		expect(sub.closed.peek()).toBeInstanceOf(Error);
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test.each([
	["fails", (answer: Lister) => answer.abort(new Error("old route failed"))],
	["ends", (answer: Lister) => answer.end()],
	["changes", (answer: Lister) => answer.add("gone", { priority: 0 })],
])("a listing follows the source that took over as the old one's listing %s", async (_, end) => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const from = a.consume();
	// The old source's listing is answered, as a session answers its peer's.
	const answer = wireOf(from).lister();
	answer.synced();
	b.createTrack("seq");
	front.serve(from);
	const tracks = front.consumer.tracks();
	try {
		expect(await tracks.next()).toMatchObject({ kind: "synced" });

		// The new source takes over just as the old source's listing settles, which a later
		// reader of it sees at the same moment the front's reader does.
		const witness = from.tracks();
		expect(await witness.next()).toMatchObject({ kind: "synced" });
		const takeover = () => front.serve(b.consume());
		const settled = witness.next().then(takeover, takeover);
		end(answer);
		await settled;
		expect(await tracks.next()).toMatchObject({ kind: "added", name: "seq" });
		b.createTrack("late");
		expect(await tracks.next()).toMatchObject({ kind: "added", name: "late" });
	} finally {
		tracks.close();
		answer.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

// A source that finished a track and closed just before the front concluded ended its listing
// after that finish, so the front's listing ends after it too.
test("a concluded front's listing keeps its closed source's finishes", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const from = a.consume();
	const answer = wireOf(from).lister();
	answer.add("seq", { priority: 0 });
	answer.add("live", { priority: 0 });
	answer.synced();
	front.serve(from);
	const tracks = front.consumer.tracks();
	try {
		for (const event of [{ kind: "added", name: "seq" }, { kind: "added", name: "live" }, { kind: "synced" }])
			expect(await tracks.next()).toMatchObject(event);
		answer.finish("seq");
		a.close();
		front.conclude();
		expect(await tracks.next()).toMatchObject({ kind: "finished", name: "seq" });
		expect(await tracks.next()).toBeUndefined();
	} finally {
		tracks.close();
		answer.close();
		front.consumer.close();
		a.close();
	}
});

test("only delivery codes are a route failing", () => {
	// Mirrors `only_delivery_codes_are_a_route_failing` in rs/moq-net/src/model/resume.rs.
	const delivery = [0x0, 0x1, 0x3, 0x4, 0x31];
	const codes = [...Array.from({ length: 0x40 }, (_, code) => code), 64, 64 + 5, 0xffffffff];
	for (const code of codes) {
		expect([code, routeFailed(new StreamError(code as StreamCode))]).toEqual([code, delivery.includes(code)]);
	}
	// The session the copy came through closed: the route went with it.
	expect(routeFailed(new SessionError(SessionCode.Internal))).toBe(true);
	// Content that failed to parse is the group's under its name.
	expect(routeFailed(new Error("malformed"))).toBe(false);
});

/**
 * A route serving `source` that records the groups it is asked to fetch, through every handle the
 * front clones from it, answering each with `answer` or as `source` would.
 */
function counted(source: BroadcastProducer, answer?: (sequence: number) => GroupConsumer | undefined) {
	const asked: number[] = [];
	class Counted extends BroadcastConsumer {
		constructor(shared?: never) {
			super(shared);
			const original = wireOf(this).fetchGroup;
			if (!original) throw new Error("a local broadcast fetches from its cache");
			overrideBroadcastWire(this, {
				fetchGroup: (name, sequence, options) => {
					asked.push(sequence);
					const answered = answer?.(sequence);
					return answered ? Promise.resolve(answered) : original(name, sequence, options);
				},
			});
		}

		override clone(): Counted {
			return new Counted(this.shareState());
		}

		static of(consumer: BroadcastConsumer): Counted {
			return new Counted((consumer as Counted).shareState());
		}
	}
	return { route: Counted.of(source.consume()), asked };
}

for (const fetched of [false, true]) {
	test(`a verdict ends a ${fetched ? "fetched" : "read"} group once, never asking its own route again`, async () => {
		const front = new Front();
		const a = new BroadcastProducer();
		const stalled = copy(a).write(0, 1, false);
		const malformed = new StreamError(StreamCode.MalformedTrack);
		// Once the group is read, a hostile route answers every fetch with a copy that fails the
		// same way.
		let hostile = false;
		const { route, asked } = counted(a, (sequence) => {
			if (!hostile) return undefined;
			const copy = new GroupProducer(sequence);
			copy.writeFrame({ payload: enc.encode(`${sequence}.0`) });
			void yieldNow().then(() => copy.abort(malformed));
			return copy.consume();
		});
		const sub = front.consumer.track("seq").subscribe();
		front.serve(route);
		const group = fetched ? await front.consumer.track("seq").fetchGroup(0) : sub.tryRecvGroup();
		if (!group) throw new Error("expected group 0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
		asked.length = 0;
		hostile = true;

		stalled.abort(malformed);
		await expect(group.readFrame()).rejects.toBe(malformed);
		await settle();
		expect(asked).toEqual([]);

		sub.close();
		front.consumer.close();
		a.close();
	});
}

test("a verdict every route repeats moves to the newer route once, then ends the group", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const stalled = copy(a).write(0, 1, false);
	const repeat = copy(b).write(0, 1, false);
	const malformed = new StreamError(StreamCode.MalformedTrack);
	const routeA = counted(a);
	const routeB = counted(b);
	front.serve(routeA.route);
	const sub = front.consumer.track("seq").subscribe();
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");

	front.serve(routeB.route);
	await settle();
	stalled.abort(malformed);
	const read = group.readFrame();
	expect(await isPending(read)).toBe(true);
	repeat.abort(malformed);
	await expect(read).rejects.toBe(malformed);
	await expect(group.readFrame()).rejects.toBe(malformed);
	expect([routeA.asked, routeB.asked]).toEqual([[], []]);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a route whose IETF session closed before subscribing moves the track to the next route", async () => {
	// The session went away: it has no request ID left to spend.
	const session = { nextRequestId: async () => undefined } as unknown as IetfSession;
	const front = new Front();
	const b = new BroadcastProducer();
	const tb = copy(b);
	front.serve(new IetfSubscriber({ session }).consume(path));
	const sub = front.consumer.track("seq").subscribe();
	await settle();

	front.serve(b.consume());
	await settle();
	tb.write(0);
	expect(drain(sub)).toEqual([0]);

	sub.close();
	front.consumer.close();
	b.close();
});
