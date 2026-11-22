import { expect, spyOn, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { race, Signal } from "@moq/signals";
import { Consumer as BroadcastConsumer, Producer as BroadcastProducer, type FetchFrom } from "./broadcast.ts";
import { defined } from "./defined.test-util.ts";
import { NotFound, SessionCode, SessionError, StreamCode, StreamError, toStreamCode } from "./error.ts";
import { type Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import { WireRoute } from "./hop.ts";
import type { Session as IetfSession } from "./ietf/session.ts";
import { Subscriber as IetfSubscriber } from "./ietf/subscriber.ts";
import { Version as IetfVersion } from "./ietf/version.ts";
import * as Instance from "./instance.ts";
import { hooks, NO_MAX_DELAY, staleHole } from "./internal.ts";
import type { Lister } from "./listing.ts";
import { Producer } from "./origin.ts";
import * as Path from "./path.ts";
import { Front, routeFailed } from "./resume.ts";
import { Timescale, Timestamp } from "./time.ts";
import type * as track from "./track.ts";
import { unwanted } from "./util/demand.ts";
import { GivenUp } from "./util/given_up.ts";
import { overrideBroadcastWire, wireOf } from "./wire.ts";

const INFO: track.Info = {
	compression: "none",
	timescale: null,
	maxAge: null,
	priority: 0,
	refresh: { ratio: null, interval: null },
};

const INSTANCE = Instance.parse("01900000-0000-7000-8000-000000000001");
const NEWER = Instance.parse("01900000-0000-7000-8000-000000000002");
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

/** A stand-in session announcing `path` under the shared instance at `cost`, serving `source`. */
function serve(origin: Producer, source: BroadcastProducer, cost: bigint) {
	const handle = wireOf(origin).receive(path, WireRoute.normalize({ instance: INSTANCE, cost }));
	void (async () => {
		for await (const request of handle.requested()) request.serve(source.consume());
	})();
	return () => handle.close();
}

/**
 * A copy of the logical track: every route's copy holds the same groups under the same sequences.
 * Resumed readers backfill superseded groups, so it sets no publisher limit.
 */
function copy(source: BroadcastProducer) {
	const producer = defined(source.createTrack("seq", { maxAge: null }));
	return {
		producer,
		write(key: bigint, frames = 1, close = true) {
			const group = producer.createGroup(key);
			for (let frame = 0; frame < frames; frame++)
				group.writeFrame({
					payload: enc.encode(`${key}.${frame}`),
					timestamp: Timestamp.unscaled(group.start),
				});
			if (close) group.finish();
			return group;
		},
	};
}

/** Expect `read` to fail as past its budget, not with some other stream error. */
async function expectOld(read: Promise<unknown>) {
	const err = await read.then(
		() => undefined,
		(e: unknown) => e,
	);
	expect(err).toBeInstanceOf(StreamError);
	expect(toStreamCode(err)).toBe(StreamCode.Old);
}

/** Every group sequence readable without waiting. */
function drain(sub: track.Subscriber): bigint[] {
	const keys: bigint[] = [];
	for (let group = sub.tryRecvGroup(); group; group = sub.tryRecvGroup()) {
		keys.push(group.start);
		group.close();
	}
	return keys;
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
	const open = tracks[0].write(0n, 1, false);
	const mirrored = spyOn(open, "mirror");
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	try {
		await clock.advance(Duration.ZERO);
		const held = sub.tryRecvGroup();
		if (!held) throw new Error("expected the held group");
		expect(dec.decode((await held.readFrame())?.payload)).toBe("0.0");
		for (let round = 0; round < 32; round++) {
			front.serve(handles[round % 2]);
			await clock.advance(Duration.ZERO);
			expect(sub.tryRecvGroup()).toBeUndefined();
			expect(mirrored.mock.calls.length).toBe(1);
		}
		tracks[1].write(1n);
		expect(drain(sub)).toEqual([1n]);
		front.serve(handles[0]);
		await clock.advance(Duration.ZERO);
		open.writeFrame({ payload: enc.encode("0.1"), timestamp: Timestamp.unscaled(open.start) });
		expect(dec.decode((await held.readFrame())?.payload)).toBe("0.1");
		tracks[0].write(2n);
		expect(drain(sub)).toEqual([2n]);
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

// Mirrors `a_front_refresh_moves_to_the_next_copy` in rs/moq-net/src/model/track.rs: a pending
// refresh outlives the copy it asked, and only the logical track closing rejects it.
test("a pending refresh moves to the next copy when the one it asked closes", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const tracks = sources.map((source) => {
		const producer = defined(source.createTrack("seq", { maxAge: null, refresh: { interval: Duration.secs(5) } }));
		const group = producer.createGroup(0n);
		group.writeFrame({ payload: enc.encode("0"), timestamp: Timestamp.unscaled(group.start) });
		group.finish();
		return producer;
	});
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe();
	try {
		await clock.advance(Duration.ZERO);
		const refresh = sub.refresh();
		expect(await isPending(refresh)).toBe(true);
		tracks[0].abort(new StreamError(StreamCode.SessionClosed));
		expect(await isPending(refresh)).toBe(true);

		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		expect(await isPending(refresh)).toBe(true);
		await clock.advance(Duration.secs(5));
		expect(tracks[1].demand().refresh.peek()).toBe(true);
		await refresh;

		const group = tracks[1].createGroup(1n);
		group.writeFrame({ payload: enc.encode("1"), timestamp: Timestamp.unscaled(group.start) });
		group.finish();
		drain(sub);
		const closed = sub.refresh().catch((err: unknown) => err);
		expect(await isPending(closed)).toBe(true);
		// The serving route loses the track: the read ends it, and the refresh with it.
		tracks[1].abort(new StreamError(StreamCode.Internal));
		const ended = await sub.recvGroup().catch((err: unknown) => err);
		expect(ended).toBeInstanceOf(StreamError);
		expect(await closed).toBe(ended);
		expect(await sub.refresh().catch((err: unknown) => err)).toBe(ended);
	} finally {
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
	const open = ta.write(0n, 1, false);
	const mirrored = spyOn(open, "mirror");
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	try {
		await clock.advance(Duration.ZERO);
		const held = sub.tryRecvGroup();
		if (!held) throw new Error("expected the held group");
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		tb.write(10n);
		tb.write(20n);
		expect(drain(sub)).toEqual([10n, 20n]);
		ta.write(15n);
		front.serve(handles[0]);
		await clock.advance(Duration.ZERO);
		expect(mirrored.mock.calls.length).toBe(1);
		expect(drain(sub)).toEqual([15n]);
		ta.write(26n);
		expect(drain(sub)).toEqual([26n]);
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
 * A route capped while replaced and then returned to reads on to the end of the reader's bounded
 * range: the cap never becomes the range's own end.
 */
test("a returning route keeps the reader's bounded end across its cap", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	const open = ta.write(0n, 1, false);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({
		delay: { max: Duration.millis(3_600_000) },
		ranges: [{ start: 0n, end: 100n }],
	});
	try {
		await clock.advance(Duration.ZERO);
		const held = sub.tryRecvGroup();
		if (!held) throw new Error("expected the held group");
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		tb.write(10n);
		tb.write(20n);
		expect(drain(sub)).toEqual([10n, 20n]);
		front.serve(handles[0]);
		await clock.advance(Duration.ZERO);
		ta.write(30n);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([30n]);
		held.close();
	} finally {
		open.finish();
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

/**
 * Replacing a resumed reader's ranges asks the serving copy for the new ones, not the endpoints
 * the old ones resolved to.
 */
test("a resumed reader's replaced ranges reach its serving copy", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	ta.write(5n);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({
		delay: { max: Duration.millis(3_600_000) },
		ranges: [{ start: 0n, end: 100n }],
	});
	try {
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([5n]);
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		tb.write(10n);
		expect(drain(sub)).toEqual([10n]);
		sub.update({
			delay: { max: Duration.millis(3_600_000) },
			ranges: [{ start: 50n, end: 60n }],
		});
		await clock.advance(Duration.ZERO);
		tb.write(30n);
		tb.write(55n);
		tb.write(70n);
		await clock.advance(Duration.ZERO);
		// 30 holds time 50, so the new range starts there; 70 is past its end.
		expect(drain(sub)).toEqual([30n, 55n]);
	} finally {
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

test("a group the window dropped above the serving copy's resume point is backfilled once ranges widen to it", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	const delay = { max: Duration.millis(3_600_000) };
	ta.write(10n);
	ta.write(50n);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({
		delay,
		ranges: [
			{ start: 10n, end: 10n },
			{ start: 50n, end: 50n },
		],
	});
	try {
		await clock.advance(Duration.ZERO);
		const first = sub.tryRecvGroup();
		expect(first?.start).toBe(10n);
		first?.close();
		// The new copy resumes at group 10 and places time 50 at group 40, the newest it holds at
		// or before it, which the reader's window leaves out: dropped above that resume point.
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		tb.write(40n);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([50n]);
		sub.update({ delay, ranges: [{ start: 40n, end: 60n }] });
		await clock.advance(Duration.ZERO);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([40n]);
	} finally {
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

test("a backfill places a time start its serving copy raised, at the group holding it", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	const delay = { max: Duration.millis(3_600_000) };
	const from = (time: bigint) => ({ delay, ranges: [{ start: time }] });
	ta.write(6n);
	ta.write(8n);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe(from(8n));
	try {
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([8n]);
		// The replacement resumes from 8, holding nothing yet, so its cache places nothing.
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		sub.update(from(7n));
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([]);
		for (const key of [6n, 8n, 9n]) tb.write(key);
		await clock.advance(Duration.ZERO);
		await clock.advance(Duration.ZERO);
		expect(drain(sub).sort()).toEqual([6n, 9n]);
		expect(hooks.servingResolved(sub)[0]?.start).toBe(6n);
	} finally {
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

/**
 * As above, beside a range still waiting on a group past the live edge: the raised start's backfill
 * is asked without waiting for that range to place. Mirrors the Rust
 * `a_backfill_places_a_raised_start_beside_a_waiting_range`.
 */
test("a backfill places a raised start beside a waiting range", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	const delay = { max: Duration.millis(3_600_000) };
	ta.write(6n);
	ta.write(8n);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ delay, ranges: [{ start: 8n }] });
	try {
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([8n]);
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		sub.update({
			delay,
			ranges: [{ start: 7n, end: 8n }, { start: 20n }],
		});
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([]);
		for (const key of [6n, 8n]) tb.write(key);
		await clock.advance(Duration.ZERO);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([6n]);
		expect(hooks.servingResolved(sub)[0]?.start).toBe(6n);
	} finally {
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

/**
 * As above, for a range that ends past the resume point: the backfill, capped under it, places only
 * the start, and the read ends at the range's own end. Mirrors the Rust
 * `a_backfill_places_a_raised_start_under_an_end_past_the_resume_point`.
 */
test("a backfill places a raised start under an end past the resume point", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	const delay = { max: Duration.millis(3_600_000) };
	ta.write(6n);
	ta.write(8n);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ delay, ranges: [{ start: 8n }] });
	try {
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([8n]);
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		sub.update({ delay, ranges: [{ start: 7n, end: 9n }] });
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([]);
		for (const key of [9n, 8n, 6n]) tb.write(key);
		tb.producer.skipGroups(7n, 8n);
		await clock.advance(Duration.ZERO);
		await clock.advance(Duration.ZERO);
		expect(drain(sub).sort()).toEqual([6n, 9n]);
		expect(hooks.servingResolved(sub)[0]).toMatchObject({ start: 6n, end: 10n });
		expect(await sub.recvGroup()).toBeUndefined();
	} finally {
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

/**
 * As a backfill placing a raised start under an end past the resume point, with the received
 * replacement behind an inner front, through which the backfill places nothing. Mirrors the Rust
 * `a_raised_start_behind_an_inner_front_hands_out_nothing_past_its_end`.
 */
test("a raised start behind an inner front hands out nothing past its end", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const inner = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	hooks.receiveTrack(tb.producer);
	const delay = { max: Duration.millis(3_600_000) };
	ta.write(6n);
	ta.write(8n);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ delay, ranges: [{ start: 8n }] });
	try {
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([8n]);
		inner.serve(handles[1]);
		front.serve(inner.consumer);
		await clock.advance(Duration.ZERO);
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		sub.update({ delay, ranges: [{ start: 7n, end: 7n }] });
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([]);
		// The backfill through the inner front places nothing, so its groups wait for the window:
		// none past the range's end goes out.
		for (const key of [6n, 7n, 8n, 9n]) {
			tb.write(key);
			await clock.advance(Duration.ZERO);
			await clock.advance(Duration.ZERO);
			expect(drain(sub)).toEqual([]);
		}
	} finally {
		sub.close();
		front.consumer.close();
		inner.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

/**
 * A backfill's group that a range already placed holds goes out though another range still waits on
 * its anchors. Mirrors the Rust `a_backfill_group_a_placed_range_holds_goes_out_beside_a_waiting_range`.
 */
test("a backfill group a placed range holds goes out beside a waiting range", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	hooks.receiveTrack(tb.producer);
	const delay = { max: Duration.millis(3_600_000) };
	ta.write(6n);
	ta.write(8n);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ delay, ranges: [{ start: 8n }] });
	try {
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([8n]);
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		sub.update({ delay, ranges: [{ start: 7n }] });
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([]);
		tb.write(7n);
		await clock.advance(Duration.ZERO);
		// A range whose start waits on a group still to land leaves the window pending.
		sub.update({ delay, ranges: [{ start: 7n }, { start: 9n }] });
		await clock.advance(Duration.ZERO);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([7n]);
	} finally {
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

/**
 * As a backfill placing a time start its serving copy raised, with the group holding the time still
 * to land when the backfill learns no group sits between it and the resume point: the backfill is
 * kept for it, below where it was asked from.
 */
test("a backfill waits for the group holding a time under its floor", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	const delay = { max: Duration.millis(3_600_000) };
	const from = (time: bigint) => ({ delay, ranges: [{ start: time }] });
	ta.write(6n);
	ta.write(8n);
	tb.write(8n);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe(from(8n));
	try {
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([8n]);
		front.serve(handles[1]);
		tb.write(9n);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([9n]);
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		sub.update(from(7n));
		tb.producer.skipGroups(7n, 8n);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([]);
		tb.write(10n);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([10n]);
		tb.write(6n);
		await clock.advance(Duration.ZERO);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([6n]);
	} finally {
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
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	const subscribed = tracks.map((track) => spyOn(track.producer, "subscribe"));
	try {
		await clock.advance(Duration.ZERO);
		for (let round = 1; round < 24; round++) {
			front.serve(handles[round % 2]);
			await clock.advance(Duration.ZERO);
			// Every odd group never lands, so every resume point leaves a gap.
			tracks[round % 2].write(BigInt(round * 2));
			await clock.advance(Duration.ZERO);
			expect(drain(sub)).toEqual([BigInt(round * 2)]);
		}
		// Each route's one backfill. Its own copy is asked again only once a cap drained it, before
		// the flaps settle on returning to the copy each route holds.
		for (const spy of subscribed) {
			const backfills = spy.mock.calls.filter(([asked]) =>
				asked?.ranges?.some((range) => range.end !== undefined),
			);
			expect(backfills.length).toBe(1);
			expect(spy.mock.calls.length).toBeLessThanOrEqual(3);
		}
		tracks[1].write(1n);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([1n]);
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
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	try {
		await clock.advance(Duration.ZERO);
		ta.write(5n);
		expect(drain(sub)).toEqual([5n]);
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([]);
		tb.write(2n);
		tb.write(6n);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([2n, 6n]);
		tb.write(4n);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([4n]);
	} finally {
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

test("a late group on a drained serving copy is read", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	hooks.receiveTrack(ta.producer);
	hooks.receiveTrack(tb.producer);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	try {
		await clock.advance(Duration.ZERO);
		ta.write(0n);
		expect(drain(sub)).toEqual([0n]);
		ta.write(2n);
		// The replaced copy, capped at 2, may still deliver 1, which the new route is asked for.
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([2n]);
		tb.write(3n);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([3n]);
		tb.producer.finish();
		const next = sub.recvGroup();
		expect(await isPending(next)).toBe(true);
		tb.write(1n);
		await clock.advance(Duration.ZERO);
		expect((await next)?.start).toBe(1n);
	} finally {
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

// A replacement that ends cleanly before delivering any group doesn't end the read while the
// replaced copy may still deliver a group the reader lacks under its cap.
test("a replacement ending before any group waits for the replaced copy's late group", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	// The old route's groups may land late; the new route's local track is complete once it ends.
	hooks.receiveTrack(ta.producer);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	try {
		await clock.advance(Duration.ZERO);
		ta.write(0n);
		ta.write(2n);
		expect(drain(sub).sort()).toEqual([0n, 2n]);
		// Group 1 is still in flight on the old route, capped at 2.
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		tb.producer.finishAt({ group: 3n, frame: 0 });
		await clock.advance(Duration.ZERO);
		const next = sub.recvGroup();
		expect(await isPending(next)).toBe(true);
		ta.write(1n);
		await clock.advance(Duration.ZERO);
		expect((await next)?.start).toBe(1n);
	} finally {
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

test("a late group under the resume point is backfilled again", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	hooks.receiveTrack(ta.producer);
	hooks.receiveTrack(tb.producer);
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	try {
		await clock.advance(Duration.ZERO);
		ta.write(2n);
		ta.producer.finish();
		expect(sub.tryRecvGroup()?.start).toBe(2n);
		// A switch away and back resumes `ta` past 2; the backfill below it finds its track
		// complete and runs out, while `tb`'s copies hold the read open.
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([]);
		front.serve(handles[0]);
		await clock.advance(Duration.ZERO);
		const next = sub.recvGroup();
		expect(await isPending(next)).toBe(true);
		// `tb` fails, and 1 lands late on `ta` before the read looks again.
		tb.producer.abort(new StreamError(StreamCode.SessionClosed));
		ta.write(1n);
		await clock.advance(Duration.ZERO);
		expect((await next)?.start).toBe(1n);
	} finally {
		sub.close();
		front.consumer.close();
		for (const handle of handles) handle.close();
		for (const source of sources) source.close();
		installed.close();
	}
});

// The switch's backfill waits on a fill the finished new route's handler still owes, rather
// than counting as drained while the fill is out.
test("a backfill waits on a fill the finished route still owes", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const sources = [new BroadcastProducer(), new BroadcastProducer()];
	const handles = sources.map((source) => source.consume());
	const [ta, tb] = sources.map(copy);
	const requests = tb.producer.requests();
	const fills: track.FillGroups[] = [];
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			// The replacement asks for the group containing the reader's newest group's start, which
			// this route never held.
			if (fill.floor < fill.start) fill.close();
			else fills.push(fill);
		}
	})();
	tb.write(6n);
	tb.producer.finish();
	front.serve(handles[0]);
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	try {
		await clock.advance(Duration.ZERO);
		ta.write(5n);
		expect(drain(sub)).toEqual([5n]);
		front.serve(handles[1]);
		await clock.advance(Duration.ZERO);
		// The route's handler first answers where the reader's newest group falls on it.
		expect(drain(sub)).toEqual([]);
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([6n]);
		await clock.advance(Duration.ZERO);
		expect(fills.length).toBeGreaterThan(0);
		expect(drain(sub)).toEqual([]);
		expect(sub.closed.peek()).toBeUndefined();
		// Only group 2 is in the route's history.
		for (const fill of fills) {
			for (let key = fill.wanted(); key !== undefined; key = fill.wanted()) {
				if (key !== 2n) {
					fill.dropGroups(key, key + 1n);
					continue;
				}
				const group = defined(fill.accept(key));
				group.writeFrame({ payload: enc.encode("2.0"), timestamp: Timestamp.unscaled(group.start) });
				group.finish();
			}
			fill.close();
		}
		await clock.advance(Duration.ZERO);
		expect(drain(sub)).toEqual([2n]);
	} finally {
		requests.close();
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
	hooks.receiveTrack(ta.producer);
	hooks.receiveTrack(tb.producer);
	hooks.startTrack(ta.producer, { start: 1n });
	hooks.startTrack(tb.producer, { start: 5n, requested: 3n });
	const front = new Front();
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ ranges: [{ start: 3n }] });
	try {
		expect(hooks.sourceStart(sub)).toBe(3n);
		const subscribed = tb.producer.subscription.changed();
		front.serve(b.consume());
		await subscribed;
		expect(hooks.sourceStart(sub)).toBe(5n);
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("a routed reader serves its live join as the serving copy resolves it", async () => {
	const a = new BroadcastProducer();
	const ta = copy(a);
	for (let key = 0n; key < 5n; key++) ta.write(key);
	const front = new Front();
	front.serve(a.consume());
	const sub = front.consumer
		.track("seq")
		.subscribe({ delay: { max: null }, ranges: [{ start: { live: Duration.ZERO }, end: 5n }] });
	try {
		await settle();
		// The copy holds the groups the join starts among, so it starts at the latest one.
		expect(hooks.servingEnd(sub)).toBe(6n);
		expect(hooks.servingRuns(sub, 0n, 10n)).toEqual([{ start: 4n, end: 6n }]);
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
	}
});

// A front's reader subscribes to a copy only once a route serves it, which may be after the copy's
// upstream answered the reader's live join. The copy takes the tick the reader added its range at,
// not its own later one, so it places the join from that answer rather than asking again. Mirrors
// the Rust fix behind `Copy::stamp`.
test("a front's reader places a live join from an answer its copy recorded before subscribing", async () => {
	const a = new BroadcastProducer();
	const ta = copy(a);
	hooks.receiveTrack(ta.producer);
	hooks.answersJoins(ta.producer);
	for (let key = 0n; key < 5n; key++) ta.write(key);
	const front = new Front();
	const sub = front.consumer
		.track("seq")
		.subscribe({ delay: { max: null }, ranges: [{ start: { live: Duration.ZERO } }] });
	try {
		// The copy's upstream answered the join after the reader added it, before any copy asked.
		hooks.answerJoin(ta.producer, 0, undefined, 3n, NO_MAX_DELAY);
		front.serve(a.consume());
		await settle();
		await settle();
		expect(drain(sub)).toEqual([3n, 4n]);
		expect(hooks.requested(ta.producer).joins()).toEqual([]);
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
	}
});

// Mirrors the Rust `Reader::exhausted`: the serving copy judges the window from its resume point,
// and a group under it the local floor keeps out stays owed.
test("a bounded read through routes ends once its window is read, across a route change", async () => {
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	for (let key = 0n; key < 3n; key++) ta.write(key);
	const front = new Front();
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ delay: { max: null }, ranges: [{ start: 1n, end: 4n }] });
	const floored = front.consumer
		.track("seq")
		.subscribe({ delay: { max: null }, ranges: [{ start: 1n, end: 4n }] })
		.withRange({ start: { included: 3n } });
	try {
		expect((await sub.recvGroup())?.start).toBe(1n);
		expect((await sub.recvGroup())?.start).toBe(2n);
		const pending = sub.recvGroup();
		expect(await Promise.race([pending, settle().then(() => "pending")])).toBe("pending");
		for (let key = 0n; key < 5n; key++) tb.write(key);
		front.serve(b.consume());
		expect((await pending)?.start).toBe(3n);
		expect((await sub.recvGroup())?.start).toBe(4n);
		// The track goes on, but the window is read.
		expect(await sub.recvGroup()).toBeUndefined();

		expect((await floored.recvGroup())?.start).toBe(3n);
		expect((await floored.recvGroup())?.start).toBe(4n);
		// Groups 1 and 2 are in range under the floor, which may still drop.
		const held = floored.recvGroup();
		expect(await Promise.race([held, settle().then(() => "pending")])).toBe("pending");
		floored.withRange({});
		expect((await held)?.start).toBe(1n);
		expect((await floored.recvGroup())?.start).toBe(2n);
		expect(await floored.recvGroup()).toBeUndefined();
	} finally {
		sub.close();
		floored.close();
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
	// A refetch from route a, should the failure land before b serves, waits on a's request
	// handler rather than failing at once.
	const pending = ta.producer.requests();
	const front = new Front();
	front.serve(a.consume());
	const parent = front.consumer.track("seq").subscribe();
	const fork = parent.fork();
	try {
		const stalled = ta.write(0n, 1, false);
		const group = await fork.recvGroup();
		if (!group) throw new Error("expected group 0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
		parent.close();

		const next = group.readFrame();
		tb.write(0n, 3);
		front.serve(b.consume());
		stalled.abort(new StreamError(StreamCode.SessionClosed));
		expect(dec.decode((await next)?.payload)).toBe("0.1");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.2");
		expect(await group.readFrame()).toBeUndefined();
	} finally {
		pending.close();
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
	ta.write(0n);
	ta.write(1n);
	expect(drain(parent)).toEqual([1n]);
	front[end]();
	const fork = parent.fork({ delay: { max: Duration.millis(10_000) } });
	try {
		expect(drain(fork)).toEqual([0n, 1n]);
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
		ranges: [{ start: 1n }],
	});
	ta.write(0n);
	ta.write(1n);
	expect(drain(parent)).toEqual([1n]);
	const fork = parent.fork({ priority: 7, delay: { max: Duration.millis(10_000) } });
	try {
		expect(drain(fork)).toEqual([0n, 1n]);
		fork.update({ priority: 9 });
		expect(parent.subscription.peek()?.priority).toBe(2);
		expect(fork.subscription.peek()?.priority).toBe(9);
		const subscribed = tb.producer.subscription.changed();
		front.serve(b.consume());
		expect((await subscribed)?.priority).toBe(9);
		const narrowed = tb.producer.subscription.changed();
		fork.close();
		expect((await narrowed)?.priority).toBe(2);
		tb.write(2n);
		expect(drain(parent)).toEqual([2n]);
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
		producer.writeFrame({ payload: enc.encode("shared"), timestamp: Timestamp.unscaled(0n) });
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

test("a lagging same-instance replica resumes from one publisher's bytes exactly once", async () => {
	const { clock, installed } = manual();
	const publisher = new Producer();
	const originalRoute = publisher.route(path, { instance: { id: INSTANCE, epoch: 0n } });
	const original = originalRoute.broadcast;
	const source = defined(original.createTrack("seq", { maxAge: null }));
	const origin = new Producer();

	// Both replicas forward this one source. Only the incumbent pauses mid-group.
	function replica(lagging: boolean) {
		const broadcast = new BroadcastProducer();
		const output = defined(broadcast.createTrack("seq", { maxAge: null }));
		const input = source.subscribe({ delay: { max: Duration.millis(60_000) } });
		const release = Promise.withResolvers<void>();
		let stopped = false;
		const done = (async () => {
			try {
				for (;;) {
					const group = await input.recvGroup();
					if (!group) return;
					const copy = output.createGroup(group.start);
					try {
						for (let index = 0; ; index++) {
							const frame = await group.readFrame();
							if (!frame) break;
							if (lagging && group.start === 1n && index === 2) await release.promise;
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
	const warm = standby.track("seq").subscribe({ delay: { max: Duration.millis(60_000) } });
	let sub: track.Subscriber | undefined;
	let disposeB: (() => void) | undefined;
	try {
		await clock.advance(Duration.nanos(0));
		const active = request.active.peek();
		if (!active) throw new Error("expected the incumbent route");
		sub = active.track("seq").subscribe({ delay: { max: Duration.millis(60_000) } });
		const first = hooks.appendNext(source);
		for (let frame = 0; frame < 4; frame++)
			first.writeFrame({ payload: enc.encode(`0.${frame}`), timestamp: Timestamp.unscaled(first.start) });
		first.finish();
		const seen: string[] = [];
		const initial = await sub.recvGroup();
		if (!initial) throw new Error("expected group 0");
		expect(initial.start).toBe(0n);
		for (let frame = 0; frame < 4; frame++) seen.push(dec.decode((await initial.readFrame())?.payload));
		expect(await initial.readFrame()).toBeUndefined();
		initial.close();

		const open = hooks.appendNext(source);
		for (let frame = 0; frame < 2; frame++)
			open.writeFrame({ payload: enc.encode(`1.${frame}`), timestamp: Timestamp.unscaled(open.start) });
		const interrupted = await sub.recvGroup();
		if (!interrupted) throw new Error("expected group 1");
		expect(interrupted.start).toBe(1n);
		for (let frame = 0; frame < 2; frame++) seen.push(dec.decode((await interrupted.readFrame())?.payload));
		for (let frame = 2; frame < 4; frame++)
			open.writeFrame({ payload: enc.encode(`1.${frame}`), timestamp: Timestamp.unscaled(open.start) });
		open.finish();
		const last = hooks.appendNext(source);
		for (let frame = 0; frame < 4; frame++)
			last.writeFrame({ payload: enc.encode(`2.${frame}`), timestamp: Timestamp.unscaled(last.start) });
		last.finish();

		// The standby holds the complete original while the incumbent cannot pass 1.1.
		for (let key = 0n; key < 3n; key++) {
			const group = await warm.recvGroup();
			if (!group) throw new Error("expected a warmed group");
			expect(group.start).toBe(key);
			for (let frame = 0; frame < 4; frame++) {
				expect(dec.decode((await group.readFrame())?.payload)).toBe(`${key}.${frame}`);
			}
			expect(await group.readFrame()).toBeUndefined();
			group.close();
		}
		disposeB = serve(origin, b.broadcast, 9n);
		await clock.advance(Duration.nanos(0));
		expect(request.active.peek()).toBe(active);
		disposeA();
		a.stop(new StreamError(StreamCode.SessionClosed));
		await clock.advance(Duration.nanos(0));
		for (let frame = 2; frame < 4; frame++) seen.push(dec.decode((await interrupted.readFrame())?.payload));
		expect(await interrupted.readFrame()).toBeUndefined();
		interrupted.close();
		const successor = await sub.recvGroup();
		if (!successor) throw new Error("expected group 2");
		expect(successor.start).toBe(2n);
		for (let frame = 0; frame < 4; frame++) seen.push(dec.decode((await successor.readFrame())?.payload));
		expect(await successor.readFrame()).toBeUndefined();
		successor.close();
		expect(seen).toEqual(Array.from({ length: 12 }, (_, index) => `${Math.floor(index / 4)}.${index % 4}`));
		await clock.advance(Duration.nanos(0));
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

test("a subscription reads on across a same-instance route swap, each group once", async () => {
	const { a, b, active, request, swap, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe({ delay: { max: Duration.millis(10_000) } });
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);

	// Both routes deliver group 1; the reader takes it once.
	ta.write(1n);
	tb.write(0n);
	tb.write(1n);
	await swap();
	expect(request.active.peek()).toBe(active);
	tb.write(2n);
	expect(drain(sub)).toEqual([1n, 2n]);

	// The replaced route's later groups are the new route's to deliver.
	ta.write(3n);
	expect(drain(sub)).toEqual([]);
	tb.write(3n);
	expect(drain(sub)).toEqual([3n]);

	sub.close();
	close();
});

test("a stalled group continues from the replacement at the next frame", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	ta.write(0n, 1, false);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");

	// The new route has the rest of it; the old one stalls.
	tb.write(0n, 3);
	await swap();
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.1");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.2");
	expect(await group.readFrame()).toBeUndefined();

	sub.close();
	close();
});

for (const closeFirst of [true, false]) {
	test(`a replaced copy's short group continues from the replacement (closeFirst=${closeFirst})`, async () => {
		const { a, b, swap, active, close } = await setup();
		const ta = copy(a);
		const tb = copy(b);
		const sub = active.track("seq").subscribe({ delay: { max: Duration.millis(300_000) } });
		const short = ta.write(0n, 1, false);
		const group = sub.tryRecvGroup();
		if (!group) throw new Error("expected group 0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
		// The old copy closes it short (FIN without GROUP_END); the new route holds the rest.
		const whole = tb.write(0n, 3, false);
		if (closeFirst) short.finish();
		await swap();
		if (!closeFirst) short.finish();
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.1");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.2");
		expect(group.closed.peek()).toBeUndefined();
		whole.finishAt(3n);
		expect(await group.readFrame()).toBeUndefined();
		expect(group.end).toBe(3n);
		expect(await group.closed).toBeNull();

		sub.close();
		close();
	});
}

// A watch no longer looking, or one that never judges, leaves a short close to the reader.
for (const watch of ["race", "givenUp"] as const) {
	test(`a short group a ${watch} watched continues from the replacement`, async () => {
		const { a, b, swap, active, close } = await setup();
		const ta = copy(a);
		const tb = copy(b);
		const sub = active.track("seq").subscribe({ delay: { max: Duration.millis(300_000) } });
		const short = ta.write(0n, 1, false);
		const group = sub.tryRecvGroup();
		if (!group) throw new Error("expected group 0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
		// A race decided by its other side drops its watch; a publisher's give-up watch stays.
		const given = watch === "givenUp" ? new GivenUp(group) : undefined;
		if (watch === "race") expect(await race([Promise.resolve("ready"), group.closed])).toBe("ready");
		const whole = tb.write(0n, 3, false);
		short.finish();
		await settle();
		await swap();
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.1");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.2");
		whole.finishAt(3n);
		expect(await group.readFrame()).toBeUndefined();
		expect(given?.peek()).toBeUndefined();
		given?.close();

		sub.close();
		close();
	});
}

test("a short group the replacement lacks reads on once it lands there", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe({ delay: { max: Duration.millis(300_000) } });
	ta.write(0n, 1);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
	await swap();
	const reading = group.readFrame();
	await settle();
	tb.write(0n, 2);
	expect(dec.decode((await reading)?.payload)).toBe("0.1");
	expect(await group.readFrame()).toBeUndefined();

	sub.close();
	close();
});

test("a short group on a failed route waits for the next one", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe({ delay: { max: Duration.millis(300_000) } });
	ta.write(0n, 1);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	ta.producer.abort(new StreamError(StreamCode.SessionClosed));
	await settle();
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
	const reading = group.readFrame();
	expect(await isPending(reading)).toBe(true);
	tb.write(0n, 2);
	await swap();
	expect(dec.decode((await reading)?.payload)).toBe("0.1");
	expect(await group.readFrame()).toBeUndefined();

	sub.close();
	close();
});

test("a short group ends once no route holds more of it", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe({ delay: { max: Duration.millis(300_000) } });
	ta.write(0n, 1);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	// The replacement closed it at the same frame.
	tb.write(0n, 1);
	await swap();
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
	expect(await group.readFrame()).toBeUndefined();

	sub.close();
	close();
});

test("a short group ends as it closed while its copy still serves", async () => {
	const { a, active, close } = await setup();
	const ta = copy(a);
	const sub = active.track("seq").subscribe({ delay: { max: Duration.millis(300_000) } });
	ta.write(0n, 1);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
	expect(group.done).toBe(true);
	expect(await group.closed).toBeNull();
	expect(await group.readFrame()).toBeUndefined();

	sub.close();
	close();
});

test("a short group ends once no route is left to ask", async () => {
	const { a, active, request, disposeA, close } = await setup();
	const ta = copy(a);
	const sub = active.track("seq").subscribe({ delay: { max: Duration.millis(300_000) } });
	ta.write(0n, 1);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	request.close();
	disposeA();
	await settle();
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
	expect(await group.readFrame()).toBeUndefined();

	sub.close();
	close();
});

test("a group whose route fails continues from the replacement", async () => {
	const { a, b, swap, active, disposeA, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	const stalled = ta.write(0n, 1, false);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");

	tb.write(0n, 2);
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
	const stalled = ta.write(0n, 1, false);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	await group.readFrame();

	// The new route never had group 0, and its group 1 is past a zero budget's reach for it.
	tb.write(1n);
	await swap();
	stalled.abort(new StreamError(StreamCode.SessionClosed));
	await expect(group.readFrame()).rejects.toBeInstanceOf(StreamError);
	expect(drain(sub)).toEqual([1n]);

	sub.close();
	close();
});

// A read that moves a group onto a new route judges it within the reader's own window: newer groups
// that route holds for others never expire it, and groups in the window do, even below where the new
// route's subscription starts. Mirrors the Rust `a_recovered_group_expires_only_within_the_readers_window`.
for (const [name, end, held, newer, expires] of [
	["outside the window", 0n, undefined, 4n, false],
	// The old route's group 4 puts the boundary past the window, so the new route's own subscription
	// asks for nothing: its groups 1 to 3 are the reader's all the same.
	["below the boundary", 3n, 4n, 3n, true],
] as const) {
	test(`a recovered group expires only within the reader's window (${name})`, async () => {
		const { a, b, swap, active, close } = await setup();
		const ta = copy(a);
		const tb = copy(b);
		const sub = active.track("seq").subscribe({ ranges: [{ start: 0n, end }] });
		ta.write(0n, 1, false);
		const group = sub.tryRecvGroup();
		if (!group) throw new Error("expected group 0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
		if (held !== undefined) ta.write(held);

		tb.write(0n, 1, false);
		await swap();
		// The stalled read moves onto the new route's copy.
		const next = group.readFrame();
		next.catch(() => {});
		expect(await isPending(next)).toBe(true);
		for (let key = 1n; key <= newer; key++) tb.write(key);
		if (expires) {
			await expectOld(next);
		} else {
			expect(await isPending(next)).toBe(true);
		}

		group.close();
		sub.close();
		close();
	});
}

// A stalled read still waiting for the new route to continue it is judged within the reader's window
// too: the new route's groups in it expire the read, though its subscription starts past them, and
// those it holds for others past the window never do. Mirrors the Rust
// `a_pending_recovery_expires_within_the_readers_window`.
for (const [name, end, held, newer, expires] of [
	["outside the window", 0n, undefined, 4n, false],
	["below the boundary", 3n, 4n, 3n, true],
] as const) {
	test(`a pending recovery expires within the reader's window (${name})`, async () => {
		const { a, b, swap, active, close } = await setup();
		const ta = copy(a);
		const tb = copy(b);
		const sub = active.track("seq").subscribe({ ranges: [{ start: 0n, end }] });
		ta.write(0n, 1, false);
		const group = sub.tryRecvGroup();
		if (!group) throw new Error("expected group 0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
		if (held !== undefined) ta.write(held);

		await swap();
		// The new route never has group 0, so the read waits on it.
		const next = group.readFrame();
		next.catch(() => {});
		expect(await isPending(next)).toBe(true);
		for (let key = 1n; key <= newer; key++) tb.write(key);
		if (expires) {
			await expectOld(next);
		} else {
			expect(await isPending(next)).toBe(true);
		}

		group.close();
		sub.close();
		close();
	});
}

// An unbounded reader judges a group moved onto a replaced copy past that copy's cap: the copy's
// newer groups, held for others, expire it.
test("a group moved onto a capped copy expires within an unbounded reader's window", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0n, 1, false);
	const group = await sub.recvGroup();
	if (group?.start !== 0n) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");

	// The switch caps A at group 0. The stalled read moves onto B, which then fails it, so it
	// continues from A, the replaced copy still holding it.
	front.serve(b.consume());
	await settle();
	const failing = tb.write(0n, 1, false);
	const next = group.readFrame();
	next.catch(() => {});
	await settle();
	failing.abort(new StreamError(StreamCode.SessionClosed));
	expect(await isPending(next)).toBe(true);

	for (let key = 1n; key <= 4n; key++) ta.write(key);
	await expectOld(next);

	group.close();
	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

// A replaced copy that fails while it still owes a group hands it to the serving route, and each read
// settles. Mirrors the Rust `a_failed_replaced_copy_backfills_once`.
test("a failed replaced copy backfills once", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ delay: { max: null } });
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);
	// Held under the cap, so the switch leaves both to the replaced copy.
	const open = ta.write(1n, 1, false);
	ta.write(2n);
	front.serve(b.consume());
	await settle();
	tb.write(1n);
	// The old route fails before group 1 went out: the serving route owes it.
	open.abort(new StreamError(StreamCode.SessionClosed));
	ta.producer.abort(new StreamError(StreamCode.SessionClosed));
	const got: bigint[] = [];
	for (let i = 0; i < 2; i++) {
		const next = sub.recvGroup();
		next.catch(() => {});
		await settle();
		expect(await isPending(next)).toBe(false);
		const group = await next;
		if (group) got.push(group.start);
	}
	expect(got.sort((x, y) => (x < y ? -1 : x > y ? 1 : 0))).toEqual([1n, 2n]);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

// A group moved onto another copy wakes when the reader's cursor cap rises over groups that expire
// it. Mirrors the Rust `a_recovered_group_wakes_when_the_readers_cap_rises`.
test("a recovered group wakes when the reader's cap rises", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active
		.track("seq")
		.subscribe()
		.withRange({ end: { excluded: 1n } });
	ta.write(0n, 1, false);
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");

	tb.write(0n, 1, false);
	await swap();
	const next = group.readFrame();
	next.catch(() => {});
	for (let key = 1n; key <= 4n; key++) tb.write(key);
	expect(await isPending(next)).toBe(true);

	sub.withRange({ end: { excluded: 5n } });
	await expectOld(next);

	group.close();
	sub.close();
	close();
});

// Moq-transport provenance is content, like Rust's `properties`: a copy over another draft differs.
for (const [name, before, after] of [
	["track properties", { priority: 1 }, { priority: 2 }],
	["moq-transport provenance", { ietf: IetfVersion.DRAFT_22 }, {}],
] as const) {
	test(`a replacement claiming different ${name} is refused once the old copy runs out`, async () => {
		const { a, b, swap, active, close } = await setup();
		const old = defined(a.createTrack("seq", before));
		defined(b.createTrack("seq", after));
		const sub = active.track("seq").subscribe();
		expect(await sub.info()).toMatchObject(before);
		await swap();
		expect(sub.closed.peek()).toBeUndefined();

		// The old copy failing with its route is what ends the track.
		old.abort(new StreamError(StreamCode.SessionClosed));
		await settle();
		expect(() => sub.tryRecvGroup()).toThrow("changed its properties");

		sub.close();
		close();
	});
}

// `unchecked` records how a copy arrived (lite-05/06), so it splices beside a lite-07 copy.
test("a replacement differing only in unchecked takes over", async () => {
	const { a, b, swap, active, close } = await setup();
	const old = defined(a.createTrack("seq", { unchecked: true, maxAge: null }));
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	await sub.info();
	await swap();
	tb.write(0n);
	old.abort(new StreamError(StreamCode.SessionClosed));
	await settle();
	expect(sub.closed.peek()).toBeUndefined();
	expect(drain(sub)).toEqual([0n]);

	sub.close();
	close();
});

// Mirrors `a_replacement_aborted_after_its_answer_keeps_the_incumbent` in rs/moq-net/src/model/origin.rs.
test("a replacement that answers and then aborts before its first group leaves the incumbent serving", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);

	await swap();
	tb.producer.abort(new StreamError(StreamCode(64)));
	await settle();
	expect(sub.closed.peek()).toBeUndefined();

	ta.write(1n);
	await settle();
	expect(drain(sub)).toEqual([1n]);

	// The incumbent's clean end is the track's, not the replacement's error.
	ta.producer.finish();
	expect(await sub.recvGroup()).toBeUndefined();

	sub.close();
	close();
});

test("a replacement that aborts before any group was held leaves the incumbent serving", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	await settle();

	await swap();
	tb.producer.abort(new StreamError(StreamCode(64)));
	await settle();
	expect(sub.closed.peek()).toBeUndefined();

	ta.write(1n);
	await settle();
	expect(drain(sub)).toEqual([1n]);

	sub.close();
	close();
});

// Mirrors `a_stranded_track_still_draining_asks_again_when_the_table_moves` in rs/moq-net/src/model/front.rs:
// a replacement losing its route judged the route, so it refuses nothing the next route serves.
test("a replacement that loses its route before its first group leaves the next route free to serve", async () => {
	const front = new Front();
	const [a, b, c] = [new BroadcastProducer(), new BroadcastProducer(), new BroadcastProducer()];
	const [ta, tb, tc] = [copy(a), copy(b), copy(c)];
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0n);
	await settle();
	expect(drain(sub)).toEqual([0n]);

	front.serve(b.consume());
	await settle();
	tb.producer.abort(new StreamError(StreamCode.Unroutable));
	await settle();
	ta.write(1n);
	await settle();
	expect(drain(sub)).toEqual([1n]);

	front.serve(c.consume());
	tc.write(2n);
	await settle();
	expect(drain(sub)).toEqual([2n]);
	expect(sub.closed.peek()).toBeUndefined();

	sub.close();
	front.consumer.close();
	for (const broadcast of [a, b, c]) broadcast.close();
});

// Mirrors the Rust front: an incumbent that finished cleanly ends the track whoever serves it.
test("a replacement that aborts before its first group after the incumbent finished ends the track cleanly", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);

	await swap();
	ta.write(1n);
	ta.producer.finish();
	tb.producer.abort(new StreamError(StreamCode(64)));
	await settle();
	const groups: bigint[] = [];
	for (let group = await sub.recvGroup(); group; group = await sub.recvGroup()) {
		groups.push(group.start);
		group.close();
	}
	expect(groups).toEqual([1n]);

	sub.close();
	close();
});

// Mirrors `readers_owed_nothing_below_the_incumbents_end_end_cleanly` in rs/moq-net/src/model/origin.rs:
// a reader owed nothing below the incumbent's end, and one joining while the replacement has yet
// to deliver, end cleanly once the incumbent finished and the replacement aborted.
test("readers owed nothing below the incumbent's end end cleanly when the replacement aborts", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const later = active.track("seq").subscribe({ ranges: [{ start: 3n }] });
	ta.write(0n);
	ta.write(1n);
	await swap();
	const joiner = active.track("seq").subscribe();
	await settle();
	ta.producer.finish();
	tb.producer.abort(new StreamError(StreamCode(64)));
	await settle();

	// Time 3 falls in group 1, the incumbent's latest, so the later reader takes it as the Rust does.
	for (const reader of [later, joiner]) {
		const groups: bigint[] = [];
		for (let group = await reader.recvGroup(); group; group = await reader.recvGroup()) {
			groups.push(group.start);
			group.close();
		}
		expect(groups).toEqual([1n]);
	}

	later.close();
	joiner.close();
	close();
});

// A reader arriving after the replacement answered reads the incumbent too, as the Rust front
// serves it until the replacement delivers.
test("a reader joining before the replacement delivers keeps the incumbent when the replacement aborts", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const first = active.track("seq").subscribe();
	ta.write(0n);
	expect(drain(first)).toEqual([0n]);
	await swap();
	const joiner = active.track("seq").subscribe();
	await settle();
	tb.producer.abort(new StreamError(StreamCode(64)));
	await settle();
	ta.write(1n);
	await settle();
	expect(drain(joiner)).toContain(1n);
	expect(joiner.closed.peek()).toBeUndefined();

	first.close();
	joiner.close();
	close();
});

// The incumbent is the source that last delivered, however many replacements answered since.
test("a reader joining after two undelivered replacements keeps the incumbent when both abort", async () => {
	const { origin, a, b, swap, active, close } = await setup();
	const c = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	const tc = copy(c);
	const first = active.track("seq").subscribe();
	ta.write(0n);
	expect(drain(first)).toEqual([0n]);
	await swap();
	const disposeC = serve(origin, c, 0n);
	await settle();
	await settle();
	const joiner = active.track("seq").subscribe();
	await settle();
	tb.producer.abort(new StreamError(StreamCode(64)));
	tc.producer.abort(new StreamError(StreamCode(64)));
	await settle();
	ta.write(1n);
	await settle();
	expect(drain(joiner)).toContain(1n);
	expect(joiner.closed.peek()).toBeUndefined();

	first.close();
	joiner.close();
	disposeC();
	close();
});

// A fork joins like any reader arriving before the replacement delivers.
test("a fork made before the replacement delivers keeps the incumbent when the replacement aborts", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);
	await swap();
	const fork = sub.fork();
	await settle();
	tb.producer.abort(new StreamError(StreamCode(64)));
	await settle();
	ta.write(1n);
	await settle();
	expect(drain(fork)).toContain(1n);
	expect(fork.closed.peek()).toBeUndefined();

	sub.close();
	fork.close();
	close();
});

// Only an end some other copy settled cleanly excuses the serving copy's failure.
test("a serving copy that declares an end and then aborts short of it keeps the error", async () => {
	const { a, active, close } = await setup();
	const ta = copy(a);
	const sub = active.track("seq").subscribe();
	ta.write(0n, 1, false);
	ta.producer.finishAt({ group: 1n, frame: 0 });
	const group = await sub.recvGroup();
	group?.close();
	ta.producer.abort(new StreamError(StreamCode(64)));
	await settle();
	await expect(sub.recvGroup()).rejects.toThrow("64");

	sub.close();
	close();
});

// Mirrors `a_replacement_aborted_after_its_answer_keeps_the_incumbent` for a reader awaiting only the end.
test("waiting for the end of a track whose replacement aborts before its first group follows the incumbent", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);

	await swap();
	const finished = sub.finished();
	finished.catch(() => {});
	tb.producer.abort(new StreamError(StreamCode(64)));
	await settle();
	ta.producer.finish();
	await expect(finished).resolves.toEqual({ group: 1n, frame: 0 });

	sub.close();
	close();
});

// The Rust front judges the replacement by its copy, whatever each reader's window took from it:
// one that held a group delivered, so its later abort is the track's verdict for every reader.
test("a replacement that delivered is not undone by a reader whose window skipped its groups", async () => {
	const { a, b, swap, active, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const first = active.track("seq").subscribe();
	// Bounded by key: a time start would resolve to the group containing it, group 0 or 1 here.
	const later = active
		.track("seq")
		.subscribe()
		.withRange({ start: { included: 3n } });
	ta.write(0n);
	expect(drain(first)).toEqual([0n]);
	expect(drain(later)).toEqual([]);

	await swap();
	tb.write(1n);
	await settle();
	expect(drain(first)).toEqual([1n]);
	tb.producer.abort(new StreamError(StreamCode(64)));
	await settle();
	expect(() => drain(later)).toThrow("64");
	ta.write(2n);
	await settle();
	expect(() => drain(first)).toThrow("64");

	first.close();
	later.close();
	close();
});

test("a replacement that aborts before its first group after the front concludes leaves the incumbent serving", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);

	front.serve(b.consume());
	await settle();
	// No front is left to replace the replacement, but the incumbent still feeds the track.
	front.conclude();
	tb.producer.abort(new StreamError(StreamCode(64)));
	await settle();
	expect(sub.closed.peek()).toBeUndefined();

	ta.write(1n);
	await settle();
	expect(drain(sub)).toEqual([1n]);
	ta.producer.finish();
	expect(await sub.recvGroup()).toBeUndefined();
	sub.close();
});

test("waiting for the end of a track whose replacement was refused ends with the refusal", async () => {
	const { a, b, swap, active, close } = await setup();
	const old = defined(a.createTrack("seq", { priority: 1 }));
	defined(b.createTrack("seq", { priority: 2 }));
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
		const failing = defined(a.createTrack("fetch")).createGroup(5n);
		const group = await front.consumer.track("fetch").fetchGroup(5n);
		const read = group.readFrame();
		const whole = defined(b.createTrack("fetch")).createGroup(5n);
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
		ta.write(0n);
		expect(drain(sub)).toEqual([0n]);

		const fa = defined(a.createTrack("fetch"));
		const failing = fa.createGroup(5n);
		failing.writeFrame({ payload: enc.encode("5.0"), timestamp: Timestamp.unscaled(failing.start) });
		const fetching = front.consumer.track("fetch").fetchGroup(5n);
		for (let turn = 0; turn < turns; turn++) await Promise.resolve();
		failing.abort(new StreamError(StreamCode.SessionClosed));
		await settle();

		// The next route holds the fetched group whole; the old one still feeds the reader a
		// group the new route never has.
		const fb = defined(b.createTrack("fetch"));
		const whole = fb.createGroup(5n);
		whole.writeFrame({ payload: enc.encode("5.0"), timestamp: Timestamp.unscaled(whole.start) });
		whole.writeFrame({ payload: enc.encode("5.1"), timestamp: Timestamp.unscaled(whole.start) });
		whole.finish();
		defined(b.createTrack("seq"));
		front.serve(b.consume());
		const group = await fetching;
		const frames: string[] = [];
		for (let frame = await group.readFrame(); frame; frame = await group.readFrame())
			frames.push(dec.decode(frame.payload));
		expect(frames).toEqual(["5.0", "5.1"]);
		group.close();
		await settle();

		ta.write(1n);
		expect(drain(sub)).toEqual([1n]);

		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("a route that goes for good ends the track as its last copy does", async () => {
	const { a, active, disposeA, request, close } = await setup();
	const ta = copy(a);
	const sub = active.track("seq").subscribe();
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);

	ta.producer.abort(new StreamError(StreamCode.SessionClosed));
	disposeA();
	await settle();
	expect(request.active.peek()).toBeUndefined();
	await expect(sub.recvGroup()).rejects.toBeInstanceOf(StreamError);

	close();
});

// A relay refuses a subscription it cannot serve with a bare Internal, which is also how a
// route fails to deliver. While that route still serves, no move will replace the copy, so
// the read has to end with the refusal rather than wait for one.
test("a route that refuses the track while still serving ends the read with its cause", async () => {
	const { a, active, close } = await setup();
	const ta = copy(a);
	const sub = active.track("seq").subscribe();
	const read = sub.recvGroup().catch((e: unknown) => e);
	ta.producer.abort(new StreamError(StreamCode.Internal));
	expect(await isPending(read)).toBe(false);
	const err = await read;
	expect(err).toBeInstanceOf(StreamError);
	expect((err as StreamError).code).toBe(StreamCode.Internal);

	close();
});

test("a copy that fails mid-serve on a route still serving is subscribed afresh", async () => {
	const { a, active, close } = await setup();
	const ta = copy(a);
	const sub = active.track("seq").subscribe();
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);

	ta.producer.abort(new StreamError(StreamCode.Internal));
	const again = copy(a);
	again.write(1n);
	const group = await sub.recvGroup();
	expect(group?.start).toBe(1n);

	sub.close();
	close();
});

// Mirrors `an_unroutable_route_strands_the_track_until_the_table_moves` in rs/moq-net/src/model/front.rs.
for (const delivered of [true, false]) {
	test(`a copy that loses its route ${delivered ? "mid-serve" : "before delivering"} waits for the next route`, async () => {
		const { a, b, active, swap, close } = await setup();
		const ta = copy(a);
		const sub = active.track("seq").subscribe();
		if (delivered) {
			ta.write(0n);
			expect(drain(sub)).toEqual([0n]);
		}

		ta.producer.abort(new StreamError(StreamCode.Unroutable));
		await settle();
		expect(sub.tryRecvGroup()).toBeUndefined();
		expect(sub.closed.peek()).toBeUndefined();

		const tb = copy(b);
		const disposeB = await swap();
		tb.write(1n);
		expect(drain(sub)).toEqual([1n]);

		sub.close();
		disposeB();
		close();
	});
}

// Mirrors `an_unroutable_route_strands_the_track_until_the_table_moves` in rs/moq-net/src/model/front.rs:
// the same route winning again once the table moves asks it afresh.
test("a read stranded on a route asks it again once the table moves", async () => {
	const { origin, a, b, active, request, close } = await setup();
	const first = copy(a);
	const sub = active.track("seq").subscribe();
	first.write(0n);
	await settle();
	expect(drain(sub)).toEqual([0n]);

	first.producer.abort(new StreamError(StreamCode.Unroutable));
	await settle();
	// The route could serve again, but reading the handle is no news: the read keeps waiting.
	const second = copy(a);
	second.write(1n);
	expect(request.active.peek()).toBe(active);
	await settle();
	expect(sub.tryRecvGroup()).toBeUndefined();
	expect(sub.closed.peek()).toBeUndefined();

	// A dearer route of the instance moves the table while the same route still wins.
	const disposeB = serve(origin, b, 10n);
	await settle();
	await settle();
	expect(drain(sub)).toEqual([1n]);

	sub.close();
	disposeB();
	close();
});

// Mirrors `a_pending_takeover_leaves_a_stranded_track_waiting` in rs/moq-net/src/model/front.rs:
// the table moving toward a cheaper route that has yet to answer is no news for the stranded one.
for (const answers of [true, false]) {
	test(`a read stranded on a route waits out a takeover still pending, until it ${answers ? "answers" : "leaves"}`, async () => {
		const { origin, a, b, active, close } = await setup();
		const first = copy(a);
		const sub = active.track("seq").subscribe();
		first.write(0n);
		await settle();
		expect(drain(sub)).toEqual([0n]);

		first.producer.abort(new StreamError(StreamCode.Unroutable));
		await settle();
		expect(sub.tryRecvGroup()).toBeUndefined();
		// The stranded route could serve again, but a cheaper one is announced and has not answered.
		const second = copy(a);
		second.write(1n);
		const next = wireOf(origin).receive(path, WireRoute.normalize({ instance: INSTANCE, cost: 1n }));
		const asked = next.requested();
		await settle();
		await settle();
		expect(sub.tryRecvGroup()).toBeUndefined();
		expect(sub.closed.peek()).toBeUndefined();

		if (answers) {
			// It answers, and takes over.
			const tb = copy(b);
			const { value: pending } = await asked.next();
			pending.serve(b.consume());
			await settle();
			await settle();
			tb.write(2n);
			await settle();
			expect(drain(sub)).toEqual([2n]);
		} else {
			// It leaves first: the stranded route wins again, which is the news the read waited for.
			next.close();
			await settle();
			await settle();
			expect(drain(sub)).toEqual([1n]);
		}

		sub.close();
		next.close();
		close();
	});
}

// The same, after another instance won the path: the request leaves the table, and its own
// instance's takeover leaving is still the news the stranded read waits for.
test("a read stranded on a route asks it again once a pending takeover leaves, after another instance won", async () => {
	const { origin, a, active, close } = await setup();
	const first = copy(a);
	const sub = active.track("seq").subscribe();
	first.write(0n);
	await settle();
	expect(drain(sub)).toEqual([0n]);

	first.producer.abort(new StreamError(StreamCode.Unroutable));
	await settle();
	expect(sub.tryRecvGroup()).toBeUndefined();
	const second = copy(a);
	second.write(1n);
	const next = wireOf(origin).receive(path, WireRoute.normalize({ instance: INSTANCE, cost: 1n }));
	await settle();
	const newer = wireOf(origin).receive(path, WireRoute.normalize({ instance: NEWER, cost: 1n }));
	await settle();
	await settle();
	expect(sub.tryRecvGroup()).toBeUndefined();

	// The takeover leaves: the stranded route wins its instance again.
	next.close();
	await settle();
	await settle();
	expect(drain(sub)).toEqual([1n]);
	expect(sub.closed.peek()).toBeUndefined();

	sub.close();
	newer.close();
	close();
});

// Mirrors the refusal half of `a_stranded_track_still_draining_asks_again_when_the_table_moves` in
// rs/moq-net/src/model/front.rs: a replacement refusing the track as unroutable judged its route.
test("a replacement that refuses a track as unroutable is asked again once the table moves, the replaced copy feeding meanwhile", async () => {
	const front = new Front();
	const [a, b] = [new BroadcastProducer(), new BroadcastProducer()];
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0n);
	await settle();
	expect(drain(sub)).toEqual([0n]);

	const requested = wireOf(b).requested();
	front.serve(b.consume());
	const request = await requested;
	if (!request) throw new Error("expected on-demand track request");
	request.reject(new StreamError(StreamCode.Unroutable));
	await settle();
	ta.write(1n);
	await settle();
	expect(drain(sub)).toEqual([1n]);

	const tb = copy(b);
	tb.write(2n);
	front.moved();
	await settle();
	expect(drain(sub)).toEqual([2n]);
	expect(sub.closed.peek()).toBeUndefined();

	sub.close();
	front.consumer.close();
	for (const broadcast of [a, b]) broadcast.close();
});

// The table moving twice in one turn, the second time toward a cheaper route that has yet to
// answer: the first move's news is stale by the time the read could act on it.
test("a read stranded on a route waits out a takeover that went pending in the same turn", async () => {
	const { origin, a, b, active, close } = await setup();
	const first = copy(a);
	const sub = active.track("seq").subscribe();
	first.write(0n);
	await settle();
	expect(drain(sub)).toEqual([0n]);

	first.producer.abort(new StreamError(StreamCode.Unroutable));
	await settle();
	expect(sub.tryRecvGroup()).toBeUndefined();
	const second = copy(a);
	second.write(1n);
	// A dearer route moves the table while the stranded one still wins, then a cheaper one does.
	const dearer = wireOf(origin).receive(path, WireRoute.normalize({ instance: INSTANCE, cost: 10n }));
	const next = wireOf(origin).receive(path, WireRoute.normalize({ instance: INSTANCE, cost: 1n }));
	const asked = next.requested();
	await settle();
	await settle();
	expect(sub.tryRecvGroup()).toBeUndefined();
	expect(sub.closed.peek()).toBeUndefined();

	const tb = copy(b);
	const { value: pending } = await asked.next();
	pending.serve(b.consume());
	await settle();
	await settle();
	tb.write(2n);
	await settle();
	expect(drain(sub)).toEqual([2n]);

	sub.close();
	dearer.close();
	next.close();
	close();
});

// A concluded front has no route left to move: the copy that serves it losing its route ends the
// read rather than strand it, even when it serves again after a replacement failed.
test("a concluded front's copy that loses its route ends the read", async () => {
	const front = new Front();
	const [a, b] = [new BroadcastProducer(), new BroadcastProducer()];
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0n);
	await settle();
	expect(drain(sub)).toEqual([0n]);

	front.serve(b.consume());
	await settle();
	tb.producer.abort(new StreamError(StreamCode.Unroutable));
	front.conclude();
	await settle();
	ta.write(1n);
	await settle();
	expect(drain(sub)).toEqual([1n]);

	ta.producer.abort(new StreamError(StreamCode.Unroutable));
	await settle();
	let ended: unknown;
	try {
		sub.tryRecvGroup();
	} catch (err) {
		ended = err;
	}
	expect(ended).toBeInstanceOf(StreamError);

	sub.close();
	front.consumer.close();
	for (const broadcast of [a, b]) broadcast.close();
});

// Mirrors `a_stranded_track_still_draining_asks_again_when_the_table_moves` in rs/moq-net/src/model/front.rs.
test("a replacement that loses its route is asked again once the table moves, the replaced copy feeding meanwhile", async () => {
	const front = new Front();
	const [a, b] = [new BroadcastProducer(), new BroadcastProducer()];
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0n);
	await settle();
	expect(drain(sub)).toEqual([0n]);

	const lost = copy(b);
	front.serve(b.consume());
	await settle();
	lost.producer.abort(new StreamError(StreamCode.Unroutable));
	await settle();
	ta.write(1n);
	await settle();
	expect(drain(sub)).toEqual([1n]);

	const tb = copy(b);
	tb.write(2n);
	front.moved();
	await settle();
	expect(drain(sub)).toEqual([2n]);
	expect(sub.closed.peek()).toBeUndefined();

	sub.close();
	front.consumer.close();
	for (const broadcast of [a, b]) broadcast.close();
});

// Mirrors `a_drained_copy_lost_as_unroutable_waits_on_the_serving_source` in rs/moq-net/src/model/front.rs.
test("a replaced copy that loses its route after its replacement did waits on the serving source", async () => {
	const front = new Front();
	const [a, b] = [new BroadcastProducer(), new BroadcastProducer()];
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0n);
	await settle();
	expect(drain(sub)).toEqual([0n]);

	const lost = copy(b);
	front.serve(b.consume());
	await settle();
	lost.producer.abort(new StreamError(StreamCode.Unroutable));
	await settle();
	ta.producer.abort(new StreamError(StreamCode.Unroutable));
	await settle();
	expect(sub.tryRecvGroup()).toBeUndefined();
	expect(sub.closed.peek()).toBeUndefined();

	// The serving source could serve again, but reading it is no news: the read keeps waiting.
	const tb = copy(b);
	tb.write(1n);
	await settle();
	expect(sub.tryRecvGroup()).toBeUndefined();

	front.moved();
	await settle();
	expect(drain(sub)).toEqual([1n]);

	sub.close();
	front.consumer.close();
	for (const broadcast of [a, b]) broadcast.close();
});

// Mirrors `a_drained_copy_lost_as_unroutable_leaves_the_serving_refusal_standing` in rs/moq-net/src/model/front.rs.
test("a replaced copy that loses its route leaves its replacement's refusal standing", async () => {
	const front = new Front();
	const [a, b] = [new BroadcastProducer(), new BroadcastProducer()];
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	await settle();

	// The replacement has no such track.
	front.serve(b.consume());
	await settle();
	ta.producer.abort(new StreamError(StreamCode.Unroutable));
	await settle();
	let ended: unknown;
	try {
		sub.tryRecvGroup();
	} catch (err) {
		ended = err;
	}
	expect(ended).toBeInstanceOf(Error);
	expect(routeFailed(ended as Error)).toBe(false);

	front.consumer.close();
	for (const broadcast of [a, b]) broadcast.close();
});

// A local broadcast's word on a track is its verdict: no route news can overtake it.
test("a local broadcast failing a track as unroutable ends the read", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume(), true);
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0n);
	await settle();
	expect(drain(sub)).toEqual([0n]);

	ta.producer.abort(new StreamError(StreamCode.Unroutable));
	await settle();
	expect(() => sub.tryRecvGroup()).toThrow();

	front.consumer.close();
	a.close();
});

test("between routes a reader keeps reading the copy it has", async () => {
	const { origin, a, b, active, request, disposeA, close } = await setup();
	const ta = copy(a);
	const tb = copy(b);
	const sub = active.track("seq").subscribe();
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);

	// The next route has not answered when the old one is retracted.
	const next = wireOf(origin).receive(path, WireRoute.normalize({ instance: INSTANCE, cost: 1n }));
	const asked = next.requested();
	disposeA();
	await settle();
	expect(request.active.peek()).toBe(active);
	expect(drain(sub)).toEqual([]);
	ta.write(1n);
	expect(drain(sub)).toEqual([1n]);

	const { value: pending } = await asked.next();
	pending.serve(b.consume());
	await settle();
	await settle();
	tb.write(1n);
	tb.write(2n);
	expect(drain(sub)).toEqual([2n]);

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
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);

	// Nothing still reads group 0 from it.
	await settle();
	front.park();
	expect(drain(sub)).toEqual([]);
	ta.write(1n);
	expect(drain(sub)).toEqual([1n]);

	// Spliced in once its copy proves compatible.
	front.serve(b.consume());
	await settle();
	tb.write(1n);
	tb.write(2n);
	expect(drain(sub)).toEqual([2n]);

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
	const stalled = ta.write(5n, 1, false);
	front.serve(a.consume());
	const group = await front.consumer.track("seq").fetchGroup(5n);
	expect(dec.decode((await group.readFrame())?.payload)).toBe("5.0");

	tb.write(5n, 2);
	front.serve(b.consume());
	stalled.abort(new StreamError(StreamCode.SessionClosed));
	expect(dec.decode((await group.readFrame())?.payload)).toBe("5.1");
	expect(await group.readFrame()).toBeUndefined();

	front.consumer.close();
	a.close();
	b.close();
});

// A wire answers a FETCH with a bare group, which carries no budget of its own: the fetch's own
// budget judges it against a reader's copy of the route, as a Rust relay judges a fetch landed in
// that copy's cache. Nothing newer is known before a reader subscribes one, and a fetch never does.
test.each([
	{ budget: "500ms", maxDelay: Duration.millis(500), stale: true, bounded: false },
	{ budget: "none", maxDelay: undefined, stale: false, bounded: false },
	// Judged against the whole track, as a fetch is, not the window of the reader it picked.
	{ budget: "500ms, beside a bounded reader", maxDelay: Duration.millis(500), stale: true, bounded: true },
])(
	"a group fetched over a wire goes stale by the fetch's own budget ($budget)",
	async ({ maxDelay, stale, bounded }) => {
		// Still until the test moves it: a scale-less track ages by arrival alone.
		const { clock, installed } = manual();
		const front = new Front();
		const a = new BroadcastProducer();
		const ta = copy(a);
		ta.write(0n);
		const { route } = counted(a, (key) => {
			const answer = new GroupProducer(key);
			answer.writeFrame({ payload: enc.encode(`${key}.0`), timestamp: Timestamp.unscaled(answer.start) });
			answer.finish();
			return answer.consume();
		});
		front.serve(route);
		try {
			const group = await front.consumer.track("seq").fetchGroup(0n, { maxDelay });
			ta.write(1000n);
			await settle();
			// Fetch-only: nothing pulls the track, so nothing newer is known here.
			expect(ta.producer.demand().wanted.peek()).toBe(false);
			expect(group.expired.peek()).toBeUndefined();
			// Listening before any copy exists, so a copy subscribed later has to wake it.
			const verdict = new Promise<Error>((resolve) => void group.expired.then(resolve));

			// A reader of group 0 alone subscribes the first copy, and a live one whose own budget is
			// far wider the next.
			const first = bounded
				? front.consumer.track("seq").subscribe({ ranges: [{ start: 0n, end: 0n }] })
				: undefined;
			const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
			await settle();
			expect(group.expired.peek()).toBeUndefined();

			// Group 0's successor arrived at the copy a second ago.
			ta.write(2000n);
			await clock.advance(SECOND);
			if (stale) expect(await verdict).toMatchObject({ code: StreamCode.Old });
			else expect(await isPending(verdict)).toBe(true);

			first?.close();
			sub.close();
			group.close();
		} finally {
			front.consumer.close();
			a.close();
			installed.close();
		}
	},
);

test("a held fetch lets go of the route it resumed onto once its reader releases the group", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	const stalled = ta.write(5n, 1, false);
	front.serve(a.consume());
	// Held, as the lite publisher asks while a FETCH reply waits for its acknowledgement.
	const held: track.FetchGroupOptions & FetchFrom = { held: true };
	const group = await front.consumer.track("seq").fetchGroup(5n, held);
	expect(dec.decode((await group.readFrame())?.payload)).toBe("5.0");

	tb.write(5n, 2);
	front.serve(b.consume());
	stalled.abort(new StreamError(StreamCode.SessionClosed));
	expect(dec.decode((await group.readFrame())?.payload)).toBe("5.1");
	expect(await group.readFrame()).toBeUndefined();
	await settle();
	expect(tb.producer.demand().wanted.peek()).toBe(true);

	group.close();
	await unwanted(tb.producer.demand());

	front.consumer.close();
	a.close();
	b.close();
});

test("a replacement spliced before the first copy answers is still checked against it", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const old = defined(a.createTrack("seq", { priority: 1 }));
	defined(b.createTrack("seq", { priority: 2 }));
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

// A front's key-order reader holds back a gap above a time start its serving copy still waits to
// place, as a plain reader does: the group containing it may land below the gap.
test("a front's ordered reader holds a gap above a waiting time start", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	hooks.receiveTrack(ta.producer);
	front.serve(a.consume());
	ta.write(2000n);
	hooks.dropTrackGroups(ta.producer, 1500n, 1600n, { error: new StreamError(StreamCode.NotFound) });
	const ordered = front.consumer
		.track("seq")
		.subscribe({ delay: { max: null }, ranges: [{ start: 1500n }] })
		.ordered();
	const read = () => {
		const out: string[] = [];
		for (let item = ordered.tryNextItem(); item; item = ordered.tryNextItem()) {
			if (item.kind === "group") out.push(`group ${item.group.start}`);
			else out.push(`gap ${item.start}..${item.end}`);
		}
		return out;
	};
	await settle();
	expect(read()).toEqual([]);
	// A parked read wakes once the start is placed.
	const next = ordered.nextItem();
	expect(await isPending(next)).toBe(true);
	hooks.timeStarts(ta.producer, [[1500n, 1000n, NO_MAX_DELAY]]);
	ta.write(1000n);
	ta.producer.skipGroups(1001n, 1500n);
	ta.producer.skipGroups(1600n, 2000n);
	const first = await next;
	expect(first?.kind === "group" ? first.group.start : first).toBe(1000n);
	expect(read()).toEqual(["gap 1500..1600", "group 2000"]);

	ordered.close();
	front.consumer.close();
	a.close();
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
		.subscribe({ delay: { max: Duration.millis(10_000) } })
		.ordered();
	ta.write(0n);
	expect((await ordered.nextGroup())?.start).toBe(0n);

	// The old copy holds group 1 unread; the new one only has group 2.
	ta.write(1n);
	tb.write(2n);
	front.serve(b.consume());
	await settle();
	expect((await ordered.nextGroup())?.start).toBe(1n);
	expect((await ordered.nextGroup())?.start).toBe(2n);

	ordered.close();
	front.consumer.close();
	a.close();
	b.close();
});

// A group the window names under the local floor never lands on a copy, so it is no reason to hold
// an ordered copy's cursor on a group the window leaves out.
test("an ordered resumed reader owes no window group under its local floor", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	ta.write(10n);
	ta.write(100n, 1, false);
	front.serve(a.consume());
	const sub = front.consumer
		.track("seq")
		.subscribe({
			delay: { max: null },
			ranges: [
				{ start: 10n, end: 10n },
				{ start: 100n, end: 200n },
			],
		})
		.withRange({ start: { included: 60n } });
	const ordered = sub.ordered();
	try {
		await settle();
		// The window resolves before any group is read.
		expect(hooks.servingResolved(sub).map((range) => range?.start)).toEqual([10n, 100n]);
		// The replacement places time 100 at group 80, the newest it holds at or before it, which the
		// window leaves out. Group 10 is still owed, but under the local floor.
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		tb.write(80n);
		tb.write(120n);
		front.serve(b.consume());
		await settle();
		expect(ordered.tryNextGroup()?.start).toBe(120n);
	} finally {
		ordered.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

// A window group the replacement declares skipped never lands on it, so it is no reason to hold
// its ordered cursor on a group the window leaves out.
test("an ordered resumed reader owes no window group its copy declares skipped", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	ta.write(10n);
	ta.write(100n, 1, false);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({
		delay: { max: null },
		ranges: [
			{ start: 10n, end: 30n },
			{ start: 100n, end: 200n },
		],
	});
	const ordered = sub.ordered();
	try {
		await settle();
		expect(ordered.tryNextGroup()?.start).toBe(10n);
		// The replacement places time 100 at group 80, which the window leaves out, and skips the
		// rest of the first range.
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		tb.producer.skipGroups(11n, 31n);
		tb.write(80n);
		tb.write(120n);
		front.serve(b.consume());
		await settle();
		expect(ordered.tryNextGroup()?.start).toBe(120n);
	} finally {
		ordered.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

// A failed copy receives nothing more, so a window group under its cursor is no reason to hold it:
// it hands out what it holds and its failure is judged.
test("an ordered resumed reader does not hold a failed copy for a group it can no longer receive", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	ta.write(10n);
	ta.write(100n, 1, false);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({
		delay: { max: null },
		ranges: [
			{ start: 10n, end: 30n },
			{ start: 100n, end: 200n },
		],
	});
	const ordered = sub.ordered();
	try {
		await settle();
		expect(ordered.tryNextGroup()?.start).toBe(10n);
		// The replacement places time 100 at group 80, which the window leaves out, then fails
		// without 11 to 30 ever landing on it.
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		tb.write(80n);
		tb.write(120n);
		front.serve(b.consume());
		await settle();
		tb.producer.abort(new StreamError(StreamCode.SessionClosed));
		await settle();
		expect(ordered.tryNextGroup()?.start).toBe(120n);
	} finally {
		ordered.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

// An ordered read whose window still owes a group below the switch boundary, which the replaced
// copy may still deliver, waits for it past the replacement's groups, and wakes when it lands.
// Mirrors the Rust `a_key_order_read_holding_a_copy_wakes_when_the_owed_group_lands`.
test("an ordered resumed reader holding a copy wakes when the owed group lands", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	hooks.receiveTrack(ta.producer);
	ta.write(10n);
	ta.write(100n, 1, false);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({
		delay: { max: null },
		ranges: [
			{ start: 10n, end: 30n },
			{ start: 100n, end: 200n },
		],
	});
	const ordered = sub.ordered();
	try {
		await settle();
		expect(ordered.tryNextGroup()?.start).toBe(10n);
		// The switch splits at 100, the largest group a holds; the window still owes the rest of
		// the first range below it, which a, capped at 100, may still deliver.
		tb.write(120n);
		front.serve(b.consume());
		await settle();
		const next = ordered.nextGroup();
		expect(await isPending(next)).toBe(true);
		ta.write(11n);
		expect(await isPending(next)).toBe(false);
		expect((await next)?.start).toBe(11n);
	} finally {
		ordered.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

// A copy's take may pass the group it offered, such as one missing its head, and return a later one:
// the reader judges the group it got against its window, not the one offered.
test("a resumed reader drops a taken group outside its window when the offered one was passed", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	// Its publisher never says where the ranges start, so the replacement places them.
	hooks.receiveTrack(ta.producer);
	ta.write(10n);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({
		delay: { max: null },
		ranges: [
			{ start: 10n, end: 10n },
			{ start: 100n, end: 200n },
		],
	});
	try {
		await settle();
		// The switch splits at 10. The replacement holds group 10 from frame 1 only, which a bounded
		// copy gives up with no fill to ask, for group 80, and places time 100 at group 100, so the
		// window leaves 80 out.
		const tb = defined(b.createTrack("seq", { maxAge: null }));
		const tail = hooks.receiveGroup(tb, { start: 10n, offset: 1 });
		tail.writeFrame({ payload: enc.encode("10.1"), timestamp: Timestamp.unscaled(10n) });
		tail.finish();
		for (const key of [80n, 100n, 120n]) {
			const group = tb.createGroup(key);
			group.writeFrame({ payload: enc.encode(`${key}.0`), timestamp: Timestamp.unscaled(key) });
			group.finish();
		}
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		front.serve(b.consume());
		await settle();
		// Group 10 still goes out from the failed copy that holds it whole, once the replacement placed
		// its range; 80 never does.
		expect(drain(sub).sort((x, y) => (x < y ? -1 : x > y ? 1 : 0))).toEqual([10n, 100n, 120n]);
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("a routed read right after an update picks its group under the new preferences", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const ordered = front.consumer
		.track("seq")
		.subscribe({ delay: { max: null } })
		.ordered();
	try {
		ta.write(0n);
		ta.write(1n);
		await settle();
		// The copy learns of the update a microtask later; the read must not pick under its old budget.
		ordered.update({ delay: { max: Duration.ZERO } });
		expect(ordered.tryNextGroup()?.start).toBe(1n);
	} finally {
		ordered.close();
		front.consumer.close();
		a.close();
	}
});

// The copy reports the owed key it passed to take the group above, and the reader returns that
// gap before the group, as a direct read of the same track does.
test("a routed ordered read returns the gap its copy passed to take a group", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	hooks.receiveTrack(ta.producer);
	front.serve(a.consume());
	const ordered = front.consumer
		.track("seq")
		.subscribe({ delay: { max: Duration.millis(1000) } })
		.ordered();
	try {
		ta.write(4n);
		expect((await ordered.nextItem())?.group?.start).toBe(4n);
		ta.write(6n);
		const held = ordered.nextItem();
		expect(await isPending(held)).toBe(true);
		await clock.advance(Duration.millis(1000));
		expect(await held).toMatchObject({ kind: "gap", start: 5n, end: 6n });
		expect((await ordered.nextItem())?.group?.start).toBe(6n);
	} finally {
		ordered.close();
		front.consumer.close();
		a.close();
		installed.close();
	}
});

for (const resolution of ["arrival", "drop", "other copy", "budget"] as const) {
	test(`an ordered resumed reader keeps an older copy's owed hole until ${resolution}`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		hooks.receiveTrack(ta.producer);
		hooks.receiveTrack(tb.producer);
		front.serve(a.consume());
		const ordered = front.consumer
			.track("seq")
			.subscribe({ delay: { max: Duration.millis(1000) } })
			.ordered();
		try {
			ta.write(4n);
			expect((await ordered.nextGroup())?.start).toBe(4n);
			ta.write(6n);
			tb.write(7n);
			// The session reports where the range the front asked from time 4 started, as with its START.
			hooks.timeStarts(tb.producer, [[4n, 7n, NO_MAX_DELAY]]);
			hooks.startTrack(tb.producer, { start: 7n, requested: 4n });
			front.serve(b.consume());
			await settle();
			const held = ordered.nextGroup();
			expect(await isPending(held)).toBe(true);
			if (resolution === "arrival") ta.write(5n);
			if (resolution === "other copy") {
				tb.write(5n);
				tb.write(6n);
			}
			if (resolution === "drop")
				hooks.dropTrackGroups(ta.producer, 5n, 6n, { error: new StreamError(StreamCode.Old) });
			if (resolution === "budget") await clock.advance(Duration.millis(1000));
			// The budget runs from when the serving copy held 7, past the boundary at 6: once it
			// passed, 6 is stale too, as on a plain read of one route.
			const expected = {
				arrival: [5n, 6n, 7n],
				"other copy": [5n, 6n, 7n],
				drop: [6n, 7n],
				budget: [7n],
			}[resolution];
			expect((await held)?.start).toBe(expected[0]);
			for (const key of expected.slice(1)) expect((await ordered.nextGroup())?.start).toBe(key);
		} finally {
			ordered.close();
			front.consumer.close();
			a.close();
			b.close();
			installed.close();
		}
	});
}

test("a late subscriber's first route is checked against properties an earlier source gave", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	defined(a.createTrack("seq", { priority: 1 }));
	defined(b.createTrack("seq", { priority: 2 }));
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
	const stalled = ta.write(0n, 1, false);
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
	const fetching = tb.producer.demand().wanted.peek();

	// Another route has it cached: the group continues there, and the fetch from b stops.
	tc.write(0n, 2);
	front.serve(c.consume());
	expect(dec.decode((await read)?.payload)).toBe("0.1");
	sub.close();
	await settle();
	expect(fetching).toBe(true);
	expect(tb.producer.demand().wanted.peek()).toBe(false);

	front.consumer.close();
	a.close();
	b.close();
	c.close();
});

// With no successor or later writes, and the fetch for the rest still unanswered: whether
// the old route failed or only stalled.
for (const fails of [true, false]) {
	test.each(["drop", "abort"] as const)(
		`a resumed group ends with the serving route's %s${fails ? " once its old route failed" : ""}`,
		async (terminal) => {
			const front = new Front();
			const a = new BroadcastProducer();
			const b = new BroadcastProducer();
			const ta = copy(a);
			const tb = copy(b);
			// Neither route has the rest of group 0: request handlers hold a refetch from a, should the
			// failure land before b serves, and from b until b drops it.
			const pending = [ta.producer.requests(), tb.producer.requests()];
			const stalled = ta.write(0n, 1, false);
			front.serve(a.consume());
			const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(60_000) } });
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
			if (terminal === "drop") hooks.dropTrackGroups(tb.producer, 0n, 1n, { error: cut });
			else tb.producer.abort(cut);
			expect(await isPending(read)).toBe(false);
			await expect(read).rejects.toBe(cut);

			for (const handler of pending) handler.close();
			sub.close();
			front.consumer.close();
			a.close();
			b.close();
		},
	);
}

/** What a resumed read with dispositions found: a group's sequence, or a drop's run and code. */
function recvDisposition(sub: track.Subscriber): bigint | [bigint, bigint, number] | string {
	const recv = hooks.tryRecvGroup(sub, true);
	if (recv.kind === "group") {
		recv.group.close();
		return recv.group.start;
	}
	if (recv.kind === "drop") return [recv.start, recv.end, (recv.error as StreamError).code];
	return recv.kind;
}

test("a resumed reader reports a drop once and still serves a late group", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(60_000) } });
	ta.write(0n);
	expect(recvDisposition(sub)).toBe(0n);
	// A route failing is no verdict: another route may still deliver the group.
	hooks.dropTrackGroups(ta.producer, 1n, 2n, { error: new StreamError(StreamCode.SessionClosed) });
	expect(recvDisposition(sub)).toBe("idle");
	hooks.dropTrackGroups(ta.producer, 2n, 4n, { error: new StreamError(StreamCode(70), { message: "cut" }) });
	expect(recvDisposition(sub)).toEqual([2n, 4n, 70]);
	expect(recvDisposition(sub)).toBe("idle");
	// Re-created under a run already reported dropped.
	ta.write(3n);
	ta.write(4n);
	expect(recvDisposition(sub)).toBe(3n);
	expect(recvDisposition(sub)).toBe(4n);
	expect(recvDisposition(sub)).toBe("idle");

	sub.close();
	front.consumer.close();
	a.close();
});

// A copy folds its record at where the reader is, so one that only reports drops while the reader
// returns gaps holds a bounded record however long it plays.
test("a route copy's record folds while the reader returns only gaps", () => {
	const copies = new Set<track.Subscriber>();
	const fold = hooks.fold;
	hooks.fold = (sub, at) => {
		copies.add(sub);
		fold(sub, at);
	};
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const ordered = front.consumer
		.track("seq")
		.subscribe({ delay: { max: null } })
		.ordered();
	try {
		ta.write(0n);
		expect(ordered.tryNextItem()).toMatchObject({ kind: "group" });
		const cut = new StreamError(StreamCode(70), { message: "cut" });
		for (let i = 1n; i <= 1000n; i++) {
			// A drop and a skip apart, so each would be a run of its own in the copy's record.
			hooks.dropTrackGroups(ta.producer, 2n * i - 1n, 2n * i, { error: cut });
			ta.producer.skipGroups(2n * i, 2n * i + 1n);
			expect(ordered.tryNextItem()).toMatchObject({ kind: "gap", start: 2n * i - 1n, end: 2n * i });
		}
		expect(copies.size).toBeGreaterThan(0);
		for (const sub of copies) expect(hooks.reportedRuns(sub)).toBeLessThanOrEqual(4);
	} finally {
		hooks.fold = fold;
		ordered.close();
		front.consumer.close();
		a.close();
	}
});

// A skip of a run the serving copy already dropped replaces the drop, so the resumed reader
// reports it again, as a skip, and only once. Mirrors Rust.
test("a resumed reader reports a skip over a reported drop", () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(60_000) } });
	const recv = () => {
		const next = hooks.tryRecvGroup(sub, true);
		if (next.kind === "group") {
			next.group.close();
			return next.group.start;
		}
		if (next.kind === "drop") return [next.start, next.end, next.skip === true];
		return next.kind;
	};
	ta.write(0n);
	expect(recv()).toBe(0n);
	hooks.dropTrackGroups(ta.producer, 1n, 3n, { error: new StreamError(StreamCode.NotFound) });
	expect(recv()).toEqual([1n, 3n, false]);
	expect(recv()).toBe("idle");
	ta.producer.skipGroups(1n, 3n);
	expect(recv()).toEqual([1n, 3n, true]);
	expect(recv()).toBe("idle");
	ta.write(3n);
	expect(recv()).toBe(3n);

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
	track.write(0n);
	expect(drain(sub)).toEqual([0n]);
	const error = new StreamError(StreamCode.NotFound);
	hooks.dropTrackGroups(track.producer, 2n, 4n, { error: error });
	expect(hooks.tryDrop(sub)).toEqual({ kind: "drop", start: 2n, end: 4n, error });
	expect(hooks.tryDrop(sub)).toBeUndefined();
	track.write(3n);
	expect(drain(sub)).toEqual([3n]);
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
		const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(1_000) } });
		expect(recvDisposition(sub)).toBe("idle");
		ta.write(0n);
		ta.write(1n);
		await clock.advance(Duration.nanos(2_000_000_000));
		expect(recvDisposition(sub)).toBe(1n);
		expect(recvDisposition(sub)).toEqual([0n, 1n, StreamCode.Old]);
		expect(recvDisposition(sub)).toBe("idle");

		sub.close();
		front.consumer.close();
		a.close();
	} finally {
		installed.close();
	}
});

// The boundary group goes out from the replaced copy holding it, whatever the serving copy says
// of it: its drop is no verdict, before or after the budget. Mirrors the Rust
// `a_reader_takes_the_boundary_group_the_serving_copy_dropped`.
test("a drop of the boundary group a replaced copy holds is no verdict", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(1_000) } });
		ta.write(0n);
		expect(recvDisposition(sub)).toBe(0n);
		// Group 1 is the boundary, still arriving on the old route.
		ta.write(1n, 1, false);
		front.serve(b.consume());
		await settle();
		// Only the new route gave group 1 up; the old one holds it.
		hooks.dropTrackGroups(tb.producer, 1n, 2n, { error: new StreamError(StreamCode(70), { message: "cut" }) });
		expect(recvDisposition(sub)).toBe(1n);
		expect(recvDisposition(sub)).toBe("idle");
		tb.write(2n);
		expect(recvDisposition(sub)).toBe(2n);
		await clock.advance(Duration.nanos(1_000_000_000));
		expect(recvDisposition(sub)).toBe("idle");

		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	} finally {
		installed.close();
	}
});

// A received stale run the reader held back is unknown again once a new upstream subscription
// supersedes it on the serving copy, so it is not reported.
test("a held-back stale run a new subscription supersedes is not reported", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(1_000) } });
		ta.write(0n);
		expect(recvDisposition(sub)).toBe(0n);
		ta.write(1n, 1, false);
		front.serve(b.consume());
		await settle();
		// The new route's upstream judged the boundary group stale; the old route holds it.
		hooks.dropTrackGroups(tb.producer, 1n, 2n, staleHole(1_000));
		expect(recvDisposition(sub)).toBe(1n);
		expect(recvDisposition(sub)).toBe("idle");
		tb.write(2n);
		expect(recvDisposition(sub)).toBe(2n);
		// The route subscribes anew before the old copy is past the budget.
		hooks.supersedeStale(tb.producer);
		await clock.advance(Duration.nanos(1_000_000_000));
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
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	ta.write(0n);
	expect(recvDisposition(sub)).toBe(0n);
	// The switch splits at 2, below which the old route may still deliver 1.
	hooks.receiveTrack(ta.producer);
	ta.write(2n, 1, false);
	front.serve(b.consume());
	await settle();
	expect(recvDisposition(sub)).toBe(2n);
	// No verdict: the copy b replaced may still deliver group 1.
	hooks.dropTrackGroups(tb.producer, 1n, 2n, { error: new StreamError(StreamCode(70), { message: "cut" }) });
	expect(recvDisposition(sub)).toBe("idle");
	front.serve(c.consume());
	await settle();
	ta.producer.abort(new StreamError(StreamCode.SessionClosed));
	tb.producer.abort(new StreamError(StreamCode.SessionClosed));
	await settle();
	expect(recvDisposition(sub)).toBe("idle");
	tc.write(1n);
	expect(recvDisposition(sub)).toBe(1n);

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
	const sub = outer.consumer.track("seq").subscribe({ delay: { max: Duration.millis(60_000) } });
	ta.write(0n);
	expect(recvDisposition(sub)).toBe(0n);
	hooks.dropTrackGroups(ta.producer, 1n, 3n, { error: new StreamError(StreamCode(70), { message: "cut" }) });
	expect(recvDisposition(sub)).toEqual([1n, 3n, 70]);
	expect(recvDisposition(sub)).toBe("idle");
	// Re-created under a run already reported dropped.
	ta.write(2n);
	ta.write(3n);
	expect(recvDisposition(sub)).toBe(2n);
	expect(recvDisposition(sub)).toBe(3n);
	expect(recvDisposition(sub)).toBe("idle");

	sub.close();
	outer.consumer.close();
	inner.consumer.close();
	a.close();
});

for (const [field, first, second] of [
	["priority", { priority: 1 }, { priority: 2 }],
	["compression", { compression: "deflate" }, { compression: "none" }],
	["refresh", { refresh: { ratio: 4 } }, { refresh: { ratio: 4, interval: Duration.secs(1) } }],
] as const) {
	test(`track info keeps one ${field} answer across routes`, async () => {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		defined(a.createTrack("seq", first));
		defined(b.createTrack("seq", second));
		front.serve(a.consume());
		expect(await front.consumer.track("seq").info()).toMatchObject(first);
		front.serve(b.consume());
		await expect(front.consumer.track("seq").info()).rejects.toThrow("changed its properties");
		front.consumer.close();
		a.close();
		b.close();
	});
}

test("a failed group on a route whose replacement was refused ends with the refusal", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = defined(a.createTrack("seq", { priority: 1 }));
	defined(b.createTrack("seq", { priority: 2 }));
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	const stalled = ta.createGroup(0n);
	stalled.writeFrame({ payload: enc.encode("0.0"), timestamp: Timestamp.unscaled(stalled.start) });
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
		const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
		const stalled = ta.write(0n, 1, false);
		const group = sub.tryRecvGroup();
		if (!group) throw new Error("expected group 0");
		await group.readFrame();

		// Like a moq-transport session: the route offers no FETCH to fill the rest from.
		if (held) tb.write(0n, 2);
		else tb.write(1n);
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

test("a fetch never reads a replacement refused for its track properties", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	defined(a.createTrack("seq", { priority: 1 }));
	const tb = defined(b.createTrack("seq", { priority: 2 }));
	const group = tb.createGroup(0n);
	group.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
	front.serve(a.consume());
	await front.consumer.track("seq").info();
	front.serve(b.consume());
	await expect(front.consumer.track("seq").fetchGroup(0n)).rejects.toThrow("changed its properties");

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
		const producer = defined(source.createTrack("seq", { priority }));
		const group = producer.createGroup(0n);
		group.writeFrame({ payload: enc.encode(`${priority}`), timestamp: Timestamp.unscaled(group.start) });
		group.finish();
	}
	front.serve(a.consume());
	(await front.consumer.track("seq").fetchGroup(0n)).close();
	front.serve(b.consume());
	await expect(front.consumer.track("seq").fetchGroup(0n)).rejects.toThrow("changed its properties");

	front.consumer.close();
	a.close();
	b.close();
});

test("aborting a fetch stops it while its route's properties are still being checked", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	defined(a.createTrack("seq"));
	// b serves tracks on demand but never answers, so its properties never arrive.
	void wireOf(b).requested();
	front.serve(a.consume());
	await front.consumer.track("seq").info();
	front.serve(b.consume());
	const abort = new AbortController();
	const fetching = front.consumer.track("seq").fetchGroup(0n, { signal: abort.signal });
	await settle();
	abort.abort(new Error("cancelled"));
	await expect(fetching).rejects.toThrow("cancelled");

	front.consumer.close();
	a.close();
	b.close();
});

test("a fetch asks its route alongside that route's properties", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	// a serves tracks on demand but never answers, so its properties never arrive.
	void wireOf(a).requested();
	const { route, asked } = counted(a);
	front.serve(route);
	const fetching = front.consumer.track("seq").fetchGroup(0n);
	void fetching.catch(() => {});
	await settle();
	expect(asked).toEqual([0n]);

	front.consumer.close();
	a.close();
});

test("a fetch never hands out a group from a route refused for its properties", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	defined(a.createTrack("seq", { priority: 1 }));
	const tb = defined(b.createTrack("seq", { priority: 2 }));
	const group = tb.createGroup(0n);
	group.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
	front.serve(a.consume());
	await front.consumer.track("seq").info();
	const { route, asked } = counted(b);
	front.serve(route);
	await expect(front.consumer.track("seq").fetchGroup(0n)).rejects.toThrow("changed its properties");
	expect(asked).toEqual([0n]);

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
	defined(b.createTrack("seq", { priority: 2 }));
	const tc = copy(c);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);
	front.serve(b.consume());
	await settle();
	front.serve(c.consume());
	await settle();
	tc.write(1n);
	expect(drain(sub)).toEqual([]);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
	c.close();
});

// A switch caps the replaced copy at the largest group any copy holds, at once: a group the old
// route gets after the switch is the new route's, which is asked from there. Mirrors the Rust
// `a_switch_caps_the_replaced_copy_at_the_largest_group_held`.
for (const ordered of [false, true]) {
	test(`a switch caps the replaced copy at the largest group held (ordered: ${ordered})`, async () => {
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe();
		const keyOrder = ordered ? sub.ordered() : undefined;
		const take = () => {
			if (!keyOrder) return drain(sub);
			const keys: bigint[] = [];
			for (let group = keyOrder.tryNextGroup(); group; group = keyOrder.tryNextGroup()) keys.push(group.start);
			return keys;
		};
		try {
			ta.write(0n);
			expect(take()).toEqual([0n]);
			// Held, though not yet handed out.
			ta.write(1n);
			front.serve(b.consume());
			await settle();
			expect(tb.producer.subscription.peek()?.ranges?.[0]?.start).toBe(1n);
			// The old route finishes the boundary group and sends nothing newer.
			expect(ta.producer.subscription.peek()?.ranges?.[0]?.end).toBe(1n);
			expect(take()).toEqual([1n]);
			ta.write(2n);
			expect(take()).toEqual([]);
			tb.write(2n);
			expect(take()).toEqual([2n]);
		} finally {
			(keyOrder ?? sub).close();
			front.consumer.close();
			a.close();
			b.close();
		}
	});
}

// A replaced copy holding a group without its head gives it up under a bounded window, so the switch
// asks the serving route for that group rather than leave it to the copy. Mirrors the Rust
// `a_switch_backfills_a_group_the_replaced_copy_holds_without_its_head`.
test("a switch backfills a group the replaced copy holds without its head", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const ordered = front.consumer
		.track("seq")
		.subscribe({ delay: { max: null }, ranges: [{ start: 0n, end: 3n }] })
		.ordered();
	try {
		expect(ordered.tryNextGroup()).toBeUndefined();
		await settle();
		const tail = hooks.receiveGroup(ta.producer, { start: 0n, offset: 1 });
		tail.writeFrame({ payload: enc.encode("0.1"), timestamp: Timestamp.unscaled(0n) });
		tail.finish();
		for (let key = 1n; key < 4n; key++) ta.write(key);
		for (let key = 0n; key < 4n; key++) tb.write(key);
		front.serve(b.consume());
		await settle();
		const items = [];
		for (let item = await ordered.nextItem(); item; item = await ordered.nextItem()) {
			items.push(item.kind === "group" ? `group ${item.group.start}` : `${item.kind} ${item.start}`);
		}
		expect(items).toEqual(["group 0", "group 1", "group 2", "group 3"]);
	} finally {
		ordered.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

// The Rust reader drops the replaced copy instead (`a_switch_before_any_group_drops_the_replaced_copy`):
// here it reads as the incumbent until the new route delivers, which bounds it.
test("a switch before any group leaves the replaced copy uncapped as the incumbent", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	try {
		expect(drain(sub)).toEqual([]);
		front.serve(b.consume());
		expect(drain(sub)).toEqual([]);
		await settle();
		// Nothing held nor handed out: nothing to split, so the new route is asked as the reader was.
		expect(tb.producer.subscription.peek()?.ranges?.[0]?.start).toBeUndefined();
		expect(ta.producer.subscription.peek()?.ranges?.[0]?.end).toBeUndefined();
		ta.write(0n);
		tb.write(1n);
		expect(drain(sub)).toEqual([0n, 1n]);
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("a group the new route replays bounds nothing, so the old route's next group still arrives", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);
	ta.write(1n);
	front.serve(b.consume());
	await settle();
	// The new route only replays group 0, which was already handed out.
	tb.write(0n);
	expect(drain(sub)).toEqual([1n]);

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
			return group?.start;
		};
		copies[0].write(0n);
		expect(await next()).toBe(0n);
		let pending = next();
		for (let route = 1; route < sources.length; route++) {
			front.serve(sources[route].consume());
			await settle();
			// Every route so far holds the next group.
			for (const held of copies.slice(0, route + 1)) held.write(BigInt(route));
			expect(await pending).toBe(BigInt(route));
			// The serving copy's group bounded the previous route's copy, so the next read lets it go.
			pending = next();
			await settle();
			expect(copies[route - 1].producer.demand().wanted.peek()).toBe(false);
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
		ta.write(0n);
		(await next())?.close();
		front.serve(b.consume());
		await settle();

		// Both routes hold group 1: the old one whole, the serving one failing after a frame.
		ta.write(1n, 2);
		const failing = tb.write(1n, 1, false);
		const fail = () => failing.abort(new StreamError(StreamCode.SessionClosed));
		if (early) fail();
		const group = await next();
		if (group?.start !== 1n) throw new Error("expected group 1");
		// Reading on must not let go of the replaced copy while the group still needs it.
		const later = next();
		await settle();
		if (!early) fail();
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.1");
		expect(await group.readFrame()).toBeUndefined();

		group.close();
		tb.write(2n);
		expect((await later)?.start).toBe(2n);
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
		ta.write(0n);
		(await next())?.close();
		front.serve(b.consume());
		await settle();

		ta.write(1n, 2);
		const failing = tb.write(1n, 1, false);
		const group = await next();
		if (group?.start !== 1n) throw new Error("expected group 1");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.0");

		// The old route goes away, and the reader reads on and sees its copy end.
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		const later = next();
		await settle();
		failing.abort(new StreamError(StreamCode.SessionClosed));
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.1");
		expect(await group.readFrame()).toBeUndefined();

		group.close();
		tb.write(2n);
		expect((await later)?.start).toBe(2n);
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
	// WireRoute a's request handler holds the refetch of its failed group open, as Rust's tests do
	// with `requests()`: without one, a miss there is NotFound at once.
	const pending = ta.producer.requests();
	const failing = ta.write(5n, 1, false);
	front.serve(a.consume());
	const group = await front.consumer.track("seq").fetchGroup(5n);
	expect(dec.decode((await group.readFrame())?.payload)).toBe("5.0");

	// Its route stays current, so its only copy of group 5 is the failed one.
	failing.abort(new StreamError(StreamCode.SessionClosed));
	const read = group.readFrame();
	// Timers still run: recovery does not spin on the failed copy.
	await settle();
	await settle();

	tb.write(5n, 2);
	front.serve(b.consume());
	expect(dec.decode((await read)?.payload)).toBe("5.1");
	expect(await group.readFrame()).toBeUndefined();

	pending.close();
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
	// WireRoute a's request handler holds the refetch of its failed group open until the next route.
	const pending = ta.producer.requests();
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	// The old route fails group 0 before the reader takes it.
	const failed = ta.write(0n, 1, false);
	failed.abort(new StreamError(StreamCode.SessionClosed));
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
	const read = group.readFrame();

	tb.write(0n, 2);
	front.serve(b.consume());
	expect(dec.decode((await read)?.payload)).toBe("0.1");

	pending.close();
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
	defined(b.createTrack("seq", { priority: 2 }));
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
	defined(b.createTrack("seq", { priority: 2 }));
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
		track.write(0n);
		const first = await sub.recvGroup();
		expect(first?.start).toBe(0n);
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
	tb.write(0n);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	front.serve(b.consume());
	const fetching = front.consumer.track("seq").fetchGroup(0n);
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
	const tb = defined(b.createTrack("seq", { priority: 2 }));
	const group = tb.createGroup(0n);
	group.writeFrame({ payload: enc.encode("b"), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	front.serve(b.consume());
	const fetching = front.consumer.track("seq").fetchGroup(0n);
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
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(10_000) } });
	ta.write(0n);
	expect(drain(sub)).toEqual([0n]);
	// Both buffered when the read starts: the lower one comes first.
	ta.write(1n);
	tb.write(2n);
	front.serve(b.consume());
	await settle();
	expect(drain(sub)).toEqual([1n, 2n]);

	sub.close();
	front.consumer.close();
	a.close();
	b.close();
});

test("a fetch still out on the old route moves to the new one", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	// a has the track but not group 5 yet, so its fetch waits on a's request handler.
	const pending = defined(a.createTrack("seq", { maxAge: null })).requests();
	const tb = copy(b);
	tb.write(5n);
	front.serve(a.consume());
	const fetching = front.consumer.track("seq").fetchGroup(5n);
	await settle();
	front.serve(b.consume());
	const group = await fetching;
	expect(dec.decode((await group.readFrame())?.payload)).toBe("5.0");

	pending.close();
	group.close();
	front.consumer.close();
	a.close();
	b.close();
});

// A front's reader reports the serving copy's largest only within its bounds: the cap it sets
// and the start its subscription names.
test("the largest honours the reader's bounds on the serving copy", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const capped = front.consumer
		.track("seq")
		.subscribe()
		.withRange({ end: { excluded: 3n } });
	const floored = front.consumer.track("seq").subscribe({ ranges: [{ start: 8n }] });
	for (let key = 0n; key < 5n; key++) ta.write(key);

	const next = capped.largest();
	expect(await isPending(next)).toBe(true);
	capped.withRange({});
	expect(await next).toEqual({ group: 4n, frame: 0 });

	// Time 8 is past the copy's edge, so it starts at group 4, the group containing it, and reports
	// that copy's largest, as the Rust `largest_honours_the_subscription_start_on_the_serving_copy`.
	expect(await floored.largest()).toEqual({ group: 4n, frame: 0 });
	ta.write(8n);
	expect(await floored.largest()).toEqual({ group: 8n, frame: 0 });

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
	ta.write(3n);
	expect(await sub.largest()).toEqual({ group: 3n, frame: 0 });
	// A publisher serving the track peeks the same edge.
	expect(hooks.peekLargest(sub).largest).toEqual({ group: 3n, frame: 0 });

	// The route fails: its copy's stream ends, the logical track's does not.
	ta.producer.abort(new StreamError(StreamCode.SessionClosed));
	let settled = false;
	const next = sub.largest().finally(() => {
		settled = true;
	});
	await settle();
	expect(settled).toBe(false);

	// The replacement starts behind what is already known.
	tb.write(2n);
	front.serve(b.consume());
	await settle();
	expect(settled).toBe(false);
	tb.write(4n);
	expect(await next).toEqual({ group: 4n, frame: 0 });

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

const SECOND = Duration.millis(1000);

test.each(["lands", "dropped", "expired"])(
	"an arrival reader gets a group still in flight on the replaced route (%s)",
	async (outcome) => {
		const { clock, installed } = manual();
		const front = new Front();
		const a = new BroadcastProducer();
		const b = new BroadcastProducer();
		const ta = copy(a);
		const tb = copy(b);
		hooks.receiveTrack(ta.producer);
		front.serve(a.consume());
		const sub = front.consumer.track("seq").subscribe({ delay: { max: SECOND } });
		try {
			ta.write(0n);
			expect(drain(sub)).toEqual([0n]);
			ta.write(2n);
			// The switch splits at 2, below which the old route may still deliver 1.
			front.serve(b.consume());
			await settle();
			tb.write(3n);
			expect(drain(sub)).toEqual([2n, 3n]);
			expect(drain(sub)).toEqual([]);
			expect(ta.producer.demand().wanted.peek()).toBe(true);

			if (outcome === "dropped") hooks.dropTrackGroups(ta.producer, 1n, 2n, { error: new NotFound("group") });
			if (outcome === "expired") await clock.advance(SECOND);
			if (outcome !== "lands") {
				// The groups drained above are released once their close settles.
				await settle();
				expect(drain(sub)).toEqual([]);
				await settle();
				expect(ta.producer.demand().wanted.peek()).toBe(false);
			}
			ta.write(1n);
			expect(drain(sub)).toEqual(outcome === "lands" ? [1n] : []);
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
	const sub = front.consumer.track("seq").subscribe({ delay: { max: SECOND } });
	try {
		ta.write(0n);
		const group = sub.tryRecvGroup();
		if (!group) throw new Error("expected group 0");
		// Read whole and released, as a publisher does before it drains the stream it wrote it to.
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
		expect(await group.readFrame()).toBeUndefined();
		group.close();
		// Its successor lands on the same copy, starting the budget there.
		const half = SECOND.div(2);
		await clock.advance(half);
		ta.write(1n);
		expect(drain(sub)).toEqual([1n]);

		let expired: Error | undefined;
		void group.expired.then((err) => {
			expired = err;
		});
		// The switch splits at 1, and the replaced copy has nothing left to drain below it: the next
		// read lets go of it, closing its subscription.
		front.serve(b.consume());
		await settle();
		tb.write(2n);
		expect(drain(sub)).toEqual([2n]);
		await settle();
		expect(ta.producer.demand().wanted.peek()).toBe(false);
		await clock.advance(half);
		await settle();
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
	const sub = front.consumer.track("seq").subscribe({ delay: { max: SECOND } });
	const next = () => {
		const recv = hooks.tryNextGroup(sub);
		if (recv.kind !== "group") return undefined;
		recv.group.close();
		return recv.group.start;
	};
	ta.write(0n);
	expect(next()).toBe(0n);
	front.serve(b.consume());
	await settle();
	tb.write(2n);
	expect(next()).toBe(2n);
	// Group 2 is released once its close settles.
	await settle();
	expect(next()).toBeUndefined();
	await settle();
	expect(ta.producer.demand().wanted.peek()).toBe(false);

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
		const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
		const view = ordered ? sub.ordered() : undefined;
		const next = () => (view ? view.nextGroup() : sub.recvGroup());
		ta.write(0n);
		(await next())?.close();
		front.serve(b.consume());
		await settle();

		const failing = tb.write(1n, 1, false);
		const group = await next();
		if (group?.start !== 1n) throw new Error("expected group 1");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.0");
		// The reader reads on before the old route has group 1 at all.
		const later = next();
		await settle();

		// The old route finishes group 1 only now, and the new route's copy of it fails.
		ta.write(1n, 2);
		failing.abort(new StreamError(StreamCode.SessionClosed));
		expect(dec.decode((await group.readFrame())?.payload)).toBe("1.1");
		expect(await group.readFrame()).toBeUndefined();

		group.close();
		tb.write(2n);
		expect((await later)?.start).toBe(2n);
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
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	const view = ordered ? sub.ordered() : undefined;
	const next = () => (view ? view.nextGroup() : sub.recvGroup());
	ta.write(0n);
	(await next())?.close();
	front.serve(b.consume());
	await settle();
	// Its route fails once replaced: closing cleanly would declare the track ends at group 1.
	ta.producer.abort(new StreamError(StreamCode.SessionClosed));
	front.serve(c.consume());
	await settle();

	const failing = tc.write(1n, 1, false);
	const group = await next();
	if (group?.start !== 1n) throw new Error("expected group 1");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("1.0");
	const later = next();
	await settle();

	// b's first group lands only now, and c's copy of it fails.
	tb.write(1n, 2);
	failing.abort(new StreamError(StreamCode.SessionClosed));
	expect(dec.decode((await group.readFrame())?.payload)).toBe("1.1");
	expect(await group.readFrame()).toBeUndefined();

	group.close();
	tc.write(2n);
	expect((await later)?.start).toBe(2n);
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
	const sub = front.consumer.track("seq").subscribe({ delay: { max: SECOND } });
	try {
		ta.write(0n);
		ta.write(2n);
		expect(drain(sub)).toEqual([0n, 2n]);

		// The new route has only the group already handed out, and ends there.
		tb.write(2n);
		tb.producer.finish();
		tb.producer.close();
		front.serve(b.consume());
		await settle();
		const next = sub.recvGroup();
		expect(await isPending(next)).toBe(true);

		ta.write(1n);
		expect((await next)?.start).toBe(1n);
		expect(await sub.recvGroup()).toBeUndefined();
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

test("a group-only reader of a finite track settles closed and lets go of its copy", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	ta.write(0n);
	// Declared but still open, so the copy's subscription is what holds the demand.
	ta.producer.finishAt({ group: 1n, frame: 0 });
	expect(drain(sub)).toEqual([0n]);
	expect(await sub.recvGroup()).toBeUndefined();
	await settle();
	expect(sub.closed.peek()).toBeNull();
	expect(ta.producer.demand().wanted.peek()).toBe(false);

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
	ta.write(0n);
	const failed = new StreamError(StreamCode(71));
	ta.producer.fail(failed);
	const group = await sub.recvGroup();
	expect(group?.start).toBe(0n);
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
	ta.write(0n);
	ta.producer.finishAt({ group: 1n, frame: 0 });
	expect(drain(fork)).toEqual([0n]);
	expect(await fork.recvGroup()).toBeUndefined();
	await settle();
	expect(fork.closed.peek()).toBeNull();
	expect(ta.producer.demand().wanted.peek()).toBe(false);

	fork.close();
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
	ta.write(0n);
	ta.producer.finishAt({ group: 2n, frame: 0 });
	await settle();

	// b serves before its subscription learns the end.
	front.serve(b.consume());
	await settle();
	expect(hooks.subscriberEnd(sub)).toEqual({ group: 2n, frame: 0 });
	expect(await sub.finished()).toEqual({ group: 2n, frame: 0 });

	// A copy declaring another end leaves the first one, and its error with it.
	// Past its live edge, which only a received copy declares with an error.
	hooks.endTrack(tb.producer, { group: 3n, frame: 0 }, new StreamError(StreamCode(70), { message: "other" }));
	await settle();
	expect(sub.closed.peek()).not.toBeInstanceOf(Error);
	expect(hooks.subscriberEnd(sub)).toEqual({ group: 2n, frame: 0 });
	expect(hooks.subscriberEndError(sub)).toBeUndefined();

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
	ta.write(0n);
	await settle();
	expect(drain(sub)).toEqual([0n]);

	front.serve(b.consume());
	await settle();
	// A session close, local or remote, cancels the copy: a failed route, not an end at group 1.
	ta.producer.abort(new SessionError(SessionCode.Cancel));
	tb.write(1n);
	await settle();
	expect(drain(sub)).toEqual([1n]);
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
	ta.write(0n);
	ta.producer.finishAt({ group: 1n, frame: 0 });
	expect(await sub.largest()).toEqual({ group: 0n, frame: 0 });
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
	ta.write(0n, 1, false);
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
	const stalled = ta.write(0n, 1, false);
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
	tb.write(0n, 2);
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
		const sub = front.consumer.track("seq").subscribe({ delay: { max: SECOND } });
		try {
			ta.write(0n);
			ta.write(2n);
			expect(drain(sub)).toEqual([0n, 2n]);

			// The new route has only the group already handed out, and ends there. Nothing reads.
			tb.write(2n);
			tb.producer.finish();
			tb.producer.close();
			front.serve(b.consume());
			await settle();
			expect(sub.closed.peek()).toBeUndefined();

			if (outcome === "lands") ta.write(1n);
			else await clock.advance(SECOND);
			await settle();
			// Settled like a direct reader's, with the group that landed still readable.
			expect(sub.closed.peek()).toBeNull();
			expect(drain(sub)).toEqual(outcome === "lands" ? [1n] : []);
			expect(await sub.recvGroup()).toBeUndefined();
			await settle();
			expect(ta.producer.demand().wanted.peek()).toBe(false);
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
		const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
		const inOrder = ordered ? sub.ordered() : undefined;
		const read = () => {
			const keys: bigint[] = [];
			const next = () => (inOrder ? inOrder.tryNextGroup() : sub.tryRecvGroup());
			for (let group = next(); group; group = next()) {
				keys.push(group.start);
				group.close();
			}
			return keys;
		};
		const asked = () => tb.producer.subscription.peek()?.ranges?.[0]?.start;
		try {
			sub.withRange({ start: { included: 4n } });
			await clock.advance(Duration.ZERO);
			for (let key = 0n; key < 6n; key++) ta.write(key);
			expect(read()).toEqual([4n, 5n]);

			if (cached) for (let key = 0n; key < 6n; key++) tb.write(key);
			front.serve(b.consume());
			await clock.advance(Duration.ZERO);
			tb.write(6n);
			expect(read()).toEqual([6n]);
			await clock.advance(Duration.ZERO);
			expect(ta.producer.demand().wanted.peek()).toBe(false);
			expect(asked()).toBe(5n);

			sub.withRange({ start: { included: 1n } });
			await clock.advance(Duration.ZERO);
			expect(asked()).toBe(ordered ? 5n : 1n);
			if (!cached) {
				expect(read()).toEqual([]);
				for (let key = 1n; key < 4n; key++) tb.write(key);
			}
			expect(read()).toEqual(ordered ? [] : [1n, 2n, 3n]);
			tb.write(7n);
			expect(read()).toEqual([7n]);
		} finally {
			sub.close();
			front.consumer.close();
			a.close();
			b.close();
			installed.close();
		}
	},
);

test("a clean end keeps the serving copy while a group read from a replaced one may continue from it", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	try {
		ta.write(0n);
		expect(drain(sub)).toEqual([0n]);
		// Group 1, the boundary, is handed out from the old route, its first frame still unread.
		const stalled = ta.write(1n, 1, false);
		front.serve(b.consume());
		await settle();
		const group = await sub.recvGroup();
		if (group?.start !== 1n) throw new Error("expected group 1");

		// The new route has all of group 1 and ends there.
		tb.write(1n, 2);
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

// Once the track ended cleanly, a copy kept for a group it may still deliver can claim another
// end without failing the reader: `closed` and the reads agree.
test("a retained copy declaring another end after a clean end fails nothing", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ delay: { max: SECOND } });
	try {
		ta.write(0n);
		ta.write(2n);
		expect(drain(sub)).toEqual([0n, 2n]);

		// The new route ends at 3, while the old one still owes group 1.
		tb.write(2n);
		tb.producer.finishAt({ group: 3n, frame: 0 });
		tb.producer.finish();
		tb.producer.close();
		front.serve(b.consume());
		await settle();
		ta.write(1n);
		await settle();
		expect(sub.closed.peek()).toBeNull();

		// The copy still holding group 1 claims another end.
		ta.producer.finishAt({ group: 5n, frame: 0 });
		ta.write(4n);
		await settle();
		expect(hooks.subscriberEnd(sub)).toEqual({ group: 3n, frame: 0 });
		expect((await sub.recvGroup())?.start).toBe(1n);
		expect(await sub.recvGroup()).toBeUndefined();
		expect(sub.closed.peek()).toBeNull();
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
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	try {
		// Group 0 is handed out while the old route is the only one.
		const stalled = ta.write(0n, 1, false);
		const group = await sub.recvGroup();
		if (group?.start !== 0n) throw new Error("expected group 0");
		expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");

		// The new route has all of group 0 and ends there.
		front.serve(b.consume());
		await settle();
		tb.write(0n, 2);
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

// A copy contradicting another's end breaks the track's identity, which hurts only this track:
// the first end stands and the reader never fails over it.
test.each(["handed out", "buffered"])("an end below a group another copy %s leaves the first end", async (seen) => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe();
	try {
		ta.write(5n);
		ta.producer.finishAt({ group: 6n, frame: 0 });
		if (seen === "handed out") expect(drain(sub)).toEqual([5n]);
		front.serve(b.consume());
		await settle();

		// The new route ends the track before a group the old one has.
		tb.producer.finishAt({ group: 3n, frame: 0 });
		await settle();
		expect(drain(sub)).toEqual(seen === "handed out" ? [] : [5n]);
		expect(await sub.finished()).toEqual({ group: 6n, frame: 0 });
		expect(sub.closed.peek()).not.toBeInstanceOf(Error);
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
	["changes", (answer: Lister) => answer.add("gone", { info: INFO })],
])("a listing follows the source that took over as the old one's listing %s", async (_, end) => {
	const front = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const from = a.consume();
	// The old source's listing is answered, as a session answers its peer's.
	const answer = wireOf(from).lister();
	answer.synced();
	defined(b.createTrack("seq"));
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
		defined(b.createTrack("late"));
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
	answer.add("seq", { info: INFO });
	answer.add("live", { info: INFO });
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

// Each source event wakes the listing's wait, which must drop the registrations it made on the
// front's signals instead of leaving a set behind per event.
test("a front's listing leaves no registration behind per event", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const from = a.consume();
	const answer = wireOf(from).lister();
	answer.synced();
	front.serve(from);
	const tracks = front.consumer.tracks();

	// Registrations still pending, by signal: one-shot, so one that fired or was disposed is gone.
	const live = new Map<Signal<unknown>, Set<object>>();
	const changed = Signal.prototype.changed;
	const spy = spyOn(Signal.prototype, "changed").mockImplementation(function (
		this: Signal<unknown>,
		fn?: (value: unknown) => void,
	) {
		const original = changed.bind(this);
		if (!fn) return original();
		const entry = {};
		const pending = live.get(this) ?? new Set();
		live.set(this, pending);
		pending.add(entry);
		const dispose = original((value: unknown) => {
			pending.delete(entry);
			fn(value);
		});
		return () => {
			pending.delete(entry);
			dispose();
		};
	} as typeof changed);
	try {
		expect(await tracks.next()).toMatchObject({ kind: "synced" });
		for (let i = 0; i < 20; i++) {
			answer.add(`track${i}`, { info: INFO });
			expect(await tracks.next()).toMatchObject({ kind: "added", name: `track${i}` });
		}
		expect(Math.max(0, ...[...live.values()].map((pending) => pending.size))).toBeLessThanOrEqual(2);
	} finally {
		spy.mockRestore();
		tracks.close();
		answer.close();
		front.consumer.close();
		a.close();
	}
});

test("only delivery codes are a route failing", () => {
	// Mirrors `only_delivery_codes_are_a_route_failing` in rs/moq-net/src/model/resume.rs.
	const delivery = [0x0, 0x1, 0x3, 0x4, 0x31, 0x36];
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
function counted(
	source: BroadcastProducer,
	answer?: (key: bigint) => GroupConsumer | Promise<GroupConsumer> | undefined,
) {
	const asked: bigint[] = [];
	class Counted extends BroadcastConsumer {
		constructor(shared?: never) {
			super(shared);
			const original = wireOf(this).fetchGroup;
			if (!original) throw new Error("a local broadcast fetches from its cache");
			overrideBroadcastWire(this, {
				fetchGroup: (name, key, options) => {
					asked.push(key);
					const answered = answer?.(key);
					return answered ? Promise.resolve(answered) : original(name, key, options);
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
	test.each(["another route", "conclusion"] as const)(
		`an unroutable ${fetched ? "fetched" : "read"} group waits for %s`,
		async (end) => {
			// Mirrors `an_unroutable_group_waits_for_another_route` in rs/moq-net/src/model/resume.rs.
			const front = new Front();
			const a = new BroadcastProducer();
			const b = new BroadcastProducer();
			const stalled = copy(a).write(0n, 1, false);
			// Once the group is read, the route still serves the track but no longer holds the group.
			let refusing = false;
			const { route, asked } = counted(a, (key) =>
				refusing ? Promise.reject(new NotFound(`group ${key}`)) : undefined,
			);
			const sub = front.consumer.track("seq").subscribe();
			front.serve(route);
			const group = fetched ? await front.consumer.track("seq").fetchGroup(0n) : sub.tryRecvGroup();
			if (!group) throw new Error("expected group 0");
			expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
			asked.length = 0;
			refusing = true;

			// Its copy loses the route: asking that route again could only answer a refusal.
			stalled.abort(new StreamError(StreamCode.Unroutable));
			const read = group.readFrame();
			expect(await isPending(read)).toBe(true);
			expect(asked).toEqual([]);

			if (end === "conclusion") {
				// No route will come: the group ends with the withdrawal, not a refusal.
				front.conclude();
				await expect(read).rejects.toMatchObject({ code: StreamCode.Unroutable });
			} else {
				// Another route holds it: the group continues there.
				copy(b).write(0n, 2);
				front.serve(b.consume());
				expect(dec.decode((await read)?.payload)).toBe("0.1");
				expect(await group.readFrame()).toBeUndefined();
			}

			sub.close();
			front.consumer.close();
			a.close();
			b.close();
		},
	);
}

test("a fetch its route already answered continues the group past that route's abort", async () => {
	// Mirrors `an_answered_fetch_outlives_the_serving_routes_abort` in rs/moq-net/src/model/resume.rs.
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = copy(a);
	const stalled = ta.write(0n, 1, false);
	// Once the group is read, the route holds each fetch until the test answers it.
	let answer: ((group: GroupConsumer) => void) | undefined;
	let holding = false;
	const { route } = counted(a, () => {
		if (!holding) return undefined;
		return new Promise<GroupConsumer>((resolve) => {
			answer = resolve;
		});
	});
	front.serve(route);
	const sub = front.consumer.track("seq").subscribe();
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
	holding = true;

	// The copy fails delivering the group: recovery asks the same route for it again.
	stalled.abort(new StreamError(StreamCode.SessionClosed));
	const read = group.readFrame();
	expect(await isPending(read)).toBe(true);
	if (!answer) throw new Error("expected a recovery fetch");

	// The route answers, then aborts its track before recovery looks again.
	const rest = new GroupProducer(0n);
	for (const frame of ["0.0", "0.1"])
		rest.writeFrame({ payload: enc.encode(frame), timestamp: Timestamp.unscaled(rest.start) });
	rest.finish();
	answer(rest.consume());
	queueMicrotask(() => ta.producer.abort(new StreamError(StreamCode(70), { message: "cut" })));
	expect(dec.decode((await read)?.payload)).toBe("0.1");
	expect(await group.readFrame()).toBeUndefined();

	sub.close();
	front.consumer.close();
	a.close();
});

for (const fetched of [false, true]) {
	test(`a verdict ends a ${fetched ? "fetched" : "read"} group once, never asking its own route again`, async () => {
		const front = new Front();
		const a = new BroadcastProducer();
		const stalled = copy(a).write(0n, 1, false);
		const malformed = new StreamError(StreamCode.MalformedTrack);
		// Once the group is read, a hostile route answers every fetch with a copy that fails the
		// same way.
		let hostile = false;
		const { route, asked } = counted(a, (key) => {
			if (!hostile) return undefined;
			const copy = new GroupProducer(key);
			copy.writeFrame({ payload: enc.encode(`${key}.0`), timestamp: Timestamp.unscaled(copy.start) });
			void yieldNow().then(() => copy.abort(malformed));
			return copy.consume();
		});
		const sub = front.consumer.track("seq").subscribe();
		front.serve(route);
		const group = fetched ? await front.consumer.track("seq").fetchGroup(0n) : sub.tryRecvGroup();
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
	const stalled = copy(a).write(0n, 1, false);
	const repeat = copy(b).write(0n, 1, false);
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
	tb.write(0n);
	expect(drain(sub)).toEqual([0n]);

	sub.close();
	front.consumer.close();
	b.close();
});

test("routed metadata receives a refusal without polling a group cursor", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const origin = new Producer();
	try {
		origin.route(Path.from("missing-track"));
		for (const request of [origin.request(Path.from("missing-track")), origin.follow(Path.from("missing-track"))]) {
			const broadcast = request.active.peek();
			if (!broadcast) throw new Error("local broadcast missing");
			const subscriber = broadcast.track("absent").subscribe();
			let outcome: unknown;
			void subscriber.info().then(
				() => {
					outcome = "accepted";
				},
				(error) => {
					outcome = error;
				},
			);
			await clock.advance(Duration.ZERO);
			expect(outcome).toBeInstanceOf(NotFound);
			subscriber.close();
			request.close();
		}
	} finally {
		origin.close();
		installed.close();
	}
});

test("a group an inner front hands out already failed still continues on the outer front's next route", async () => {
	const inner = new Front();
	const outer = new Front();
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	inner.serve(a.consume());
	outer.serve(inner.consumer);
	const sub = outer.consumer.track("seq").subscribe();
	const failed = ta.write(0n, 1, false);
	// The outer front switches routes before the reader takes the group: the inner front has
	// no other route, so only the outer one may continue it.
	outer.serve(b.consume());
	failed.abort(new StreamError(StreamCode(70), { message: "cut" }));
	await settle();
	const group = sub.tryRecvGroup();
	if (!group) throw new Error("expected group 0");
	expect(dec.decode((await group.readFrame())?.payload)).toBe("0.0");
	const read = group.readFrame();
	tb.write(0n, 2);
	expect(dec.decode((await read)?.payload)).toBe("0.1");
	expect(await group.closed).toBeNull();

	sub.close();
	outer.consumer.close();
	inner.consumer.close();
	a.close();
	b.close();
});

// Mirrors `a_routed_reader_places_a_range_past_one_still_waiting` in rs/moq-net/src/model/resume.rs:
// a range the copy placed is placed on the reader too while an earlier one still waits on a time
// start, so a publisher names its start.
test("a routed reader places a range past one still waiting", async () => {
	const a = new BroadcastProducer();
	const ta = copy(a);
	hooks.receiveTrack(ta.producer);
	const front = new Front();
	front.serve(a.consume());
	ta.write(64n);
	const sub = front.consumer
		.track("seq")
		.subscribe({ delay: { max: null }, ranges: [{ start: 66n }, { start: 64n }] });
	try {
		await settle();
		const resolved = hooks.servingResolved(sub);
		expect(resolved[0]).toBeUndefined();
		expect(resolved[1]?.start).toBe(64n);
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
	}
});

// Mirrors `an_ended_copy_leaves_a_time_start_to_the_serving_copy` in rs/moq-net/src/model/resume.rs:
// a copy that failed holds nothing past its last group, so it reaches a time start past that group
// back to its oldest. The copy continuing the track places it where the track holds it.
test("a failed copy leaves a time start to the serving copy", async () => {
	const a = new BroadcastProducer();
	const b = new BroadcastProducer();
	const ta = copy(a);
	const tb = copy(b);
	hooks.receiveTrack(ta.producer);
	hooks.receiveTrack(tb.producer);
	const front = new Front();
	front.serve(a.consume());
	ta.write(64n);
	const waiting = front.consumer.track("seq").subscribe({ delay: { max: null }, ranges: [{ start: 66n }] });
	try {
		const read: bigint[] = [];
		const reading = (async () => {
			for (let group = await waiting.recvGroup(); group; group = await waiting.recvGroup()) {
				read.push(group.start);
				group.close();
				if (read.length === 2) return;
			}
		})();
		await settle();
		// The source fails before the group holding the start arrives.
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		await settle();
		expect(read).toEqual([]);
		// Nor does the reader report a start the failed copy placed.
		expect(hooks.servingResolved(waiting)[0]).toBeUndefined();
		expect(hooks.servingStarts(waiting, 0n)[0]).toBeUndefined();
		front.serve(b.consume());
		await settle();
		// Fetched ahead of the live feed's 66.
		tb.write(67n);
		tb.write(66n);
		await reading;
		expect(read.sort()).toEqual([66n, 67n]);
		expect(hooks.servingResolved(waiting)[0]?.start).toBe(66n);
	} finally {
		waiting.close();
		front.consumer.close();
		a.close();
		b.close();
	}
});

// A range from the oldest group needs no copy to place it, so a failed copy's buffered group goes
// out without waiting for a replacement, as the Rust reader's window starting unheld places it.
test("a failed copy hands out its buffered group under ranges no copy places", async () => {
	const a = new BroadcastProducer();
	const ta = copy(a);
	hooks.receiveTrack(ta.producer);
	const front = new Front();
	front.serve(a.consume());
	const sub = front.consumer.track("seq").subscribe({ delay: { max: null } });
	try {
		ta.write(64n);
		ta.producer.abort(new StreamError(StreamCode.SessionClosed));
		await settle();
		const group = sub.tryRecvGroup();
		expect(group?.start).toBe(64n);
		group?.close();
	} finally {
		sub.close();
		front.consumer.close();
		a.close();
	}
});

// A failed copy holding a group back for a placement still reports its failure: a route still
// serving is asked afresh, and that copy places the start, rather than the read waiting on a copy
// that can deliver nothing more. It got a group, so its route did not refuse the track.
test("a failed copy holding a group back is still subscribed afresh", async () => {
	const { a, active, close } = await setup();
	const ta = copy(a);
	hooks.receiveTrack(ta.producer);
	ta.write(64n);
	const sub = active.track("seq").subscribe({ delay: { max: null }, ranges: [{ start: 66n }] });
	await settle();
	expect(sub.tryRecvGroup()).toBeUndefined();

	ta.producer.abort(new StreamError(StreamCode.Internal));
	const again = copy(a);
	hooks.receiveTrack(again.producer);
	again.write(66n);
	again.write(67n);
	const read: bigint[] = [];
	for (let attempt = 0; attempt < 10 && read.length < 2; attempt++) {
		await settle();
		for (let group = sub.tryRecvGroup(); group; group = sub.tryRecvGroup()) {
			read.push(group.start);
			group.close();
		}
	}
	expect(read).toEqual([66n, 67n]);
	expect(sub.closed.peek()).toBeUndefined();

	sub.close();
	close();
});

// As above, with the fresh copy failing too while it holds a group back: the read asks again.
test("failed copies holding groups back are each subscribed afresh", async () => {
	const { a, active, close } = await setup();
	const ta = copy(a);
	hooks.receiveTrack(ta.producer);
	ta.write(64n);
	const sub = active.track("seq").subscribe({ delay: { max: null }, ranges: [{ start: 66n }] });
	const read: bigint[] = [];
	const poll = async () => {
		for (let attempt = 0; attempt < 10 && read.length < 2; attempt++) {
			await settle();
			for (let group = sub.tryRecvGroup(); group; group = sub.tryRecvGroup()) {
				read.push(group.start);
				group.close();
			}
		}
	};
	await poll();
	expect(read).toEqual([]);

	ta.producer.abort(new StreamError(StreamCode.Internal));
	const retry = copy(a);
	hooks.receiveTrack(retry.producer);
	retry.write(65n);
	await poll();
	expect(read).toEqual([]);

	retry.producer.abort(new StreamError(StreamCode.Internal));
	const again = copy(a);
	hooks.receiveTrack(again.producer);
	again.write(66n);
	again.write(67n);
	await poll();
	expect(read).toEqual([66n, 67n]);
	expect(sub.closed.peek()).toBeUndefined();

	sub.close();
	close();
});

// Mirrors the Rust `a_time_end_between_groups_ends_an_ordered_read_at_the_next` integration test,
// through a front serving the publisher's own track: its copy passes on what that publisher created.
test("a time end between two groups ends an ordered read through a front at the next", async () => {
	const front = new Front();
	const source = new BroadcastProducer();
	const handle = source.consume();
	front.serve(handle);
	const track = copy(source);
	for (const key of [1000n, 2000n, 3000n]) track.write(key);
	const between = {
		ranges: [{ start: 1500n, end: 1999n }],
		delay: { max: Duration.millis(3_600_000) },
	};
	const ordered = front.consumer.track("seq").subscribe(between).ordered();
	expect((await ordered.nextGroup())?.start).toBe(1000n);
	expect(await ordered.nextGroup()).toBeUndefined();
	// An arrival read waits: the publisher may still create 1500.
	const arrival = front.consumer.track("seq").subscribe(between);
	expect((await arrival.recvGroup())?.start).toBe(1000n);
	const waiting = arrival.recvGroup();
	expect(await isPending(waiting)).toBe(true);
	// The ended ordered read passed its range: a group created there now is late.
	track.write(1500n);
	expect(await ordered.nextGroup()).toBeUndefined();
	expect((await waiting)?.start).toBe(1500n);
	ordered.close();
	arrival.close();
	front.consumer.close();
	handle.close();
	source.close();
});

// An ordered read through a front that ends past a group its copy gave up, as one missing its head
// with no fill to ask, returns that gap before the end.
test("an ordered read through a front returns the gap its copy gave up before ending", async () => {
	const front = new Front();
	const a = new BroadcastProducer();
	const ta = defined(a.createTrack("seq", { maxAge: null }));
	const tail = hooks.receiveGroup(ta, { start: 0n, offset: 1 });
	tail.writeFrame({ payload: enc.encode("0.1"), timestamp: Timestamp.unscaled(0n) });
	tail.finish();
	const past = ta.createGroup(2n);
	past.writeFrame({ payload: enc.encode("2.0"), timestamp: Timestamp.unscaled(2n) });
	past.finish();
	front.serve(a.consume());
	const ordered = front.consumer
		.track("seq")
		.subscribe({ delay: { max: null }, ranges: [{ start: 0n, end: 1n }] })
		.ordered();
	try {
		expect(await ordered.nextItem()).toMatchObject({ kind: "gap", start: 0n, end: 1n });
		expect(await ordered.nextItem()).toBeUndefined();
	} finally {
		ordered.close();
		front.consumer.close();
		a.close();
	}
});

// A routed reader knows a whole group's span from its serving copy, as the Rust
// `Subscriber::span_end` asks it, so a publisher reading through it names none of the starts
// inside the span.
test("a routed reader reports its serving copy's whole group spans", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const front = new Front();
	const source = new BroadcastProducer();
	const handle = source.consume();
	const producer = defined(source.createTrack("seq", { ...INFO, timescale: Timescale.MILLI }));
	const group = producer.createGroup(3n);
	group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(3) });
	group.finishAt(10n);
	front.serve(handle);
	const sub = front.consumer.track("seq").subscribe({ delay: { max: Duration.millis(3_600_000) } });
	try {
		await clock.advance(Duration.ZERO);
		expect(sub.tryRecvGroup()?.start).toBe(3n);
		expect(hooks.wholeEnd(sub, 3n)).toBe(10n);
		expect(hooks.wholeAround(sub, 5n)).toEqual({ start: 3n, end: 10n });
		expect(hooks.wholeAround(sub, 10n)).toBeUndefined();
	} finally {
		sub.close();
		front.consumer.close();
		handle.close();
		source.close();
		installed.close();
	}
});
