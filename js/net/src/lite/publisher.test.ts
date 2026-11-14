import { expect, mock, spyOn, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Signal } from "@moq/signals";
import type { Grant } from "../auth.ts";
import { Producer as BroadcastProducer } from "../broadcast.ts";
import { fromTransport, ProtocolViolation, SessionCode, StreamCode, StreamError, toStreamCode } from "../error.ts";
import type { Producer as GroupProducer } from "../group.ts";
import { randomHop } from "../hop.ts";
import { hooks, staleHole } from "../internal.ts";
import { createMockTransportPair, manual, settlesWithin, textFrame } from "../mock.ts";
import { lines } from "../open.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { sendOrder } from "../priority.ts";
import { Front } from "../resume.ts";
import { Reader, Stream, Writer, writeBudget } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import { Subscriber as TrackSubscriber } from "../track.ts";
import { wireOf } from "../wire.ts";
import {
	type AnnounceBroadcast,
	AnnounceOk,
	type AnnounceReply,
	AnnounceRequest,
	decodeAnnounceBroadcast,
	encodeAnnounceReply,
} from "./announce.ts";
import { Fetch } from "./fetch.ts";
import { Group as GroupMessage } from "./group.ts";
import { Probe as ProbeMessage } from "./probe.ts";
import { Publisher } from "./publisher.ts";
import {
	decodeSubscribeResponse,
	decodeSubscribeResponseMaybe,
	Subscribe,
	type SubscribeFinish,
	SubscribeUpdate,
} from "./subscribe.ts";
import { TrackInfo, Track as TrackMessage } from "./track.ts";
import { ALPN_05, ALPN_06, ALPN_07_WIP, hasLengthFirst, hasRanges, keepsSubscribeOpen, Version } from "./version.ts";

function publish(origin: OriginProducer, path: Path.Valid) {
	const broadcastRoute = origin.route(path);
	const broadcast = broadcastRoute.broadcast;
	return broadcast;
}

// Scheduling tests intentionally stall groups, so keep latency enforcement out of their scope.
const TEST_MAX_DELAY_MS = 30_000;

function replaySubscribe(props: ConstructorParameters<typeof Subscribe>[0]) {
	return new Subscribe({ ...props, maxDelay: TEST_MAX_DELAY_MS });
}

function replayUpdate(props: ConstructorParameters<typeof SubscribeUpdate>[0]) {
	return new SubscribeUpdate({ ...props, maxDelay: TEST_MAX_DELAY_MS });
}

// A lite-07 update that keeps its ranges, which edits none of them.
function keep(version: Version) {
	return hasRanges(version) ? { ranges: [] } : {};
}

// A lite-07 update restating its span retires the range it replaces, the one with Range ID `id`.
function restate(version: Version, id = 0n) {
	return hasRanges(version) ? { remove: [id] } : {};
}

test.each([Version.DRAFT_01, Version.DRAFT_03, Version.DRAFT_06])(
	"an initial announcement write failure disposes its listener in version %s",
	async (version) => {
		const pair = createMockTransportPair(ALPN_05);
		const origin = new OriginProducer();
		const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
		const broadcast = publish(origin, Path.from("static"));
		await Promise.resolve();
		const failure = new Error("peer stopped receiving announcements");
		const stream = new Stream({
			version: version,
			readable: new ReadableStream<Uint8Array>(),
			writable: new WritableStream<Uint8Array>({
				write() {
					throw failure;
				},
			}),
		});
		const changed = Signal.prototype.changed;
		const disposed = mock(() => {});
		const registration = spyOn(Signal.prototype, "changed").mockImplementationOnce(function (
			this: Signal<unknown>,
			fn?: (value: unknown) => void,
		) {
			const original = changed.bind(this);
			if (!fn) return original();
			const dispose = original(fn);
			return () => {
				dispose();
				disposed();
			};
		} as typeof changed);
		try {
			await expect(publisher.runAnnounce(new AnnounceRequest(Path.empty()), stream)).rejects.toThrow(
				failure.message,
			);
			expect(registration).toHaveBeenCalledTimes(1);
			expect(disposed).toHaveBeenCalledTimes(1);
		} finally {
			registration.mockRestore();
			publisher.close();
			broadcast.close();
			origin.close();
			stream.close();
			pair.client.close();
			pair.server.close();
		}
	},
);

test.each([
	[Version.DRAFT_05, false],
	[Version.DRAFT_06, true],
])("a re-price goes out only where the wire carries cost (version %s)", async (version, sent) => {
	using clock = manual();
	const pair = createMockTransportPair(ALPN_05);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
	const broadcastRoute = origin.route(Path.from("cam"), { cost: 7n });
	const broadcast = broadcastRoute.broadcast;

	const written: Uint8Array[] = [];
	const stream = new Stream({
		version: version,
		readable: new ReadableStream<Uint8Array>(),
		writable: new WritableStream<Uint8Array>({
			write(chunk) {
				written.push(chunk);
			},
		}),
	});
	const settle = () => clock.advance(Duration.ZERO);
	const running = publisher.runAnnounce(new AnnounceRequest(Path.empty()), stream);
	await settle();
	const initial = written.length;
	expect(initial).toBeGreaterThan(0);

	broadcastRoute.setCost(9n);
	await settle();
	expect(written.length > initial).toBe(sent);

	stream.close();
	await running;
	publisher.close();
	broadcast.close();
	origin.close();
	pair.client.close();
	pair.server.close();
});

test.each([Version.DRAFT_06, Version.DRAFT_07])(
	"a republish restarts the advertisement (version %s)",
	async (version) => {
		using clock = manual();
		const pair = createMockTransportPair(ALPN_05);
		const origin = new OriginProducer();
		const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
		const old = publish(origin, Path.from("cam"));

		const written: Uint8Array[] = [];
		const stream = new Stream({
			version: version,
			readable: new ReadableStream<Uint8Array>(),
			writable: new WritableStream<Uint8Array>({
				write(chunk) {
					written.push(new Uint8Array(chunk));
				},
			}),
		});
		const settle = () => clock.advance(Duration.ZERO);
		const running = publisher.runAnnounce(new AnnounceRequest(Path.empty()), stream);
		await settle();
		const initial = written.length;

		// Another broadcast at the path, without an instance: another instance.
		const republished = publish(origin, Path.from("cam"));
		await settle();
		const bytes = Uint8Array.from(written.slice(initial).flatMap((chunk) => [...chunk]));
		const reader = new Reader(undefined, bytes, version);
		const messages: AnnounceBroadcast[] = [];
		while (!(await reader.done())) messages.push(await decodeAnnounceBroadcast(reader, version));
		if (version === Version.DRAFT_07) {
			// One batch under the route, closed by its floor.
			expect(messages).toMatchObject([
				{ status: "restart", id: 0n },
				{ status: "floor", id: 0n },
			]);
		} else {
			expect(messages).toMatchObject([
				{ status: "endedId", id: 0n },
				{ status: "active", suffix: "cam" },
			]);
		}

		stream.close();
		await running;
		publisher.close();
		republished.close();
		old.close();
		origin.close();
		pair.client.close();
		pair.server.close();
	},
);

// Delivers `sequences` in the given order, finishes the track (with `error`, if given), and
// returns the SUBSCRIBE_FINISH the publisher put on the wire.
async function subscribeEnd(
	sequences: number[],
	version: Version = Version.DRAFT_05,
	error?: Error,
): Promise<SubscribeFinish> {
	using clock = manual();
	const pair = createMockTransportPair(version === Version.DRAFT_07 ? ALPN_07_WIP : ALPN_05);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());

	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const client = await Stream.open(pair.client, { version });
	const server = await Stream.accept(pair.server, version);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 });
	void publisher.runSubscribe(msg, server);

	// Finish the track only once it's being served, so the publisher observes a live
	// track ending rather than resolving a subscribe against an already-closed one.
	for (const sequence of sequences) {
		const group = track.createGroup(BigInt(sequence));
		group.writeFrame(textFrame("hello", Timestamp.fromMillis(sequence)));
		group.finish();

		// Let the publisher drain this group before the next, so arrival order is the
		// order given rather than whatever the cache hands over in one batch. A group that
		// arrives late opens no stream at all, so there is nothing firmer to wait on.
		await clock.advance(Duration.ZERO);
	}
	if (error) track.finishAt({ group: BigInt(Math.max(-1, ...sequences) + 1), frame: 0 }, error);
	else track.finish();
	track.close();

	try {
		for (;;) {
			const resp = await decodeSubscribeResponse(client.reader, version);
			if ("finish" in resp) return resp.finish;
		}
	} finally {
		publisher.close();
		client.close();
	}
}

// Wait for the publisher to start serving the next group.
//
// The peer end of the stream arrives while the publisher is still opening it, so that alone
// says nothing about the ranking. Its first byte does: the header is written only after the
// group has joined the subscription's ranking.
async function servingNextGroup(opened: ReadableStreamDefaultReader<ReadableStream<Uint8Array>>) {
	const next = await opened.read();
	if (next.done) throw new Error("publisher never opened the group stream");

	const reader = next.value.getReader();
	if ((await reader.read()).done) throw new Error("publisher never wrote the group header");
	reader.releaseLock();
}

// Serves `sequences` under the given subscription, optionally raising its priority via
// SUBSCRIBE_UPDATE after the first group, and returns the send order of each group stream in
// the order the publisher opened them.
//
// Every group is left open, so all of their streams are in flight and ranked against each
// other. A group that finished would leave the ranking, which is the point of it.
async function groupSendOrders(options: {
	priority: number;
	sequences: number[];
	update?: number;
	publisher?: number;
	version?: Version;
}) {
	const { priority, sequences, update, version = Version.DRAFT_05 } = options;
	const groups: GroupProducer[] = [];
	const pair = createMockTransportPair(ALPN_05);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());

	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", {
		timescale: Timescale.MILLI,
		priority: options.publisher,
		maxAge: null,
	});

	const client = await Stream.open(pair.client, { version });
	const server = await Stream.accept(pair.server, version);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = replaySubscribe({
		id: 0n,
		broadcast: Path.from("test"),
		track: "video",
		priority,
	});
	void publisher.runSubscribe(msg, server);

	// The peer end of each group stream, which arrives as the publisher opens it.
	const opened = pair.client.incomingUnidirectionalStreams.getReader();

	try {
		for (const [index, sequence] of sequences.entries()) {
			if (index === 1 && update !== undefined) {
				await replayUpdate({ priority: update, ...keep(version) }).encode(client.writer, version);
				// Wait for the publisher to apply it, rather than assuming it beat the next group.
				while (track.subscription.peek()?.priority !== update) await track.subscription.changed();
			}

			const group = track.createGroup(BigInt(sequence));
			groups.push(group);
			group.writeFrame(textFrame("hello"));

			// One stream per group, in the order given: wait for this one before queuing the next.
			await servingNextGroup(opened);
		}

		return pair.server.sendStreams.uni.map((stream) => {
			if (stream.sendOrder === undefined) throw new Error("group stream opened without a send order");
			return stream.sendOrder;
		});
	} finally {
		opened.releaseLock();
		for (const group of groups) group.finish();
		publisher.close();
		client.close();
	}
}

// Without a send order every group stream ranks the same, so the transport round-robins them
// and a stalled low-priority track steals bandwidth from the one the subscriber asked for.
// Live playback wants the newest group, so it is the one at position 0.
test("lite draft-05: group streams are ranked newest-first", async () => {
	const orders = await groupSendOrders({ priority: 7, sequences: [0, 1, 2] });
	expect(orders).toEqual([
		sendOrder({ priority: 7, position: 2 }),
		sendOrder({ priority: 7, position: 1 }),
		sendOrder({ priority: 7, position: 0 }),
	]);

	expect(orders[2]).toBeGreaterThan(orders[1]);
});

// Two tracks the subscriber values equally each get their next group out, whatever their group
// numbering: ranking by sequence would let the one with larger numbers starve the other.
test("lite draft-05: equal priorities tie regardless of group numbering", async () => {
	const [fresh, ongoing] = await Promise.all([
		groupSendOrders({ priority: 7, sequences: [0, 1] }),
		groupSendOrders({ priority: 7, sequences: [900_000, 900_001] }),
	]);

	expect(fresh).toEqual(ongoing);
});

// The track's publisher priority breaks ties on every draft, as in Rust, including the drafts
// before lite-05 that acknowledge with SUBSCRIBE_OK instead of TRACK_INFO.
test.each([Version.DRAFT_03, Version.DRAFT_05])("lite %s: the publisher priority ranks each group", async (version) => {
	const orders = await groupSendOrders({ priority: 7, publisher: 5, sequences: [0], version });
	expect(orders).toEqual([sendOrder({ priority: 7, publisher: 5 })]);
});

// SUBSCRIBE_UPDATE re-ranks the subscription, so every group it is serving picks up the new
// priority, including one opened after the update landed.
test("lite draft-05: a subscribe update re-ranks the whole subscription", async () => {
	const orders = await groupSendOrders({ priority: 1, sequences: [0, 1], update: 9 });
	expect(orders).toEqual([sendOrder({ priority: 9, position: 1 }), sendOrder({ priority: 9, position: 0 })]);
});

// A group can outlive the priority it opened with, so an update has to reach the stream that
// is already on the wire. Otherwise (say) an active-speaker change waits for the next group.
test("lite draft-05: a subscribe update re-ranks a group already on the wire", async () => {
	using clock = manual();
	const pair = createMockTransportPair(ALPN_05);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomHop(), origin.consume());

	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const client = await Stream.open(pair.client, { version: Version.DRAFT_05 });
	const server = await Stream.accept(pair.server, Version.DRAFT_05);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = replaySubscribe({
		id: 0n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 1,
	});
	void publisher.runSubscribe(msg, server);

	// Leave the group open, so its stream is still being served when the update lands.
	const group = track.createGroup(4n);
	group.writeFrame(textFrame("hello"));

	const opened = pair.client.incomingUnidirectionalStreams.getReader();
	await servingNextGroup(opened);
	opened.releaseLock();

	// The only group in flight, so it is the one to send next.
	const stream = pair.server.sendStreams.uni[0];
	expect(stream.sendOrder).toBe(sendOrder({ priority: 1 }));

	try {
		await replayUpdate({ priority: 9 }).encode(client.writer, Version.DRAFT_05);
		while (track.subscription.peek()?.priority !== 9) await track.subscription.changed();

		// The publisher re-ranks from that same signal, so let everything it queued run rather
		// than depend on the dispatch order between its subscriber and this one.
		await clock.advance(Duration.ZERO);

		expect(stream.sendOrder).toBe(sendOrder({ priority: 9 }));
	} finally {
		group.finish();
		publisher.close();
		client.close();
	}
});

// Opening a stream can block on transport capacity, and a subscription listener only sees
// later changes, so an update landing in that window would otherwise be lost until the next one.
test("lite draft-05: a subscribe update during the stream open still ranks the group", async () => {
	using clock = manual();
	const pair = createMockTransportPair(ALPN_05);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomHop(), origin.consume());

	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	// Hold the group's stream open call until the test releases it.
	let release: () => void = () => {};
	const opening = new Promise<void>((resolve) => {
		release = resolve;
	});
	const open = pair.server.createUnidirectionalStream.bind(pair.server);
	pair.server.createUnidirectionalStream = async (options) => {
		await opening;
		return open(options);
	};

	const client = await Stream.open(pair.client, { version: Version.DRAFT_05 });
	const server = await Stream.accept(pair.server, Version.DRAFT_05);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = replaySubscribe({
		id: 0n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 1,
	});
	void publisher.runSubscribe(msg, server);

	const group = track.createGroup(4n);
	group.writeFrame(textFrame("hello"));

	try {
		// The publisher is now parked inside the open, having already read priority 1.
		await replayUpdate({ priority: 9 }).encode(client.writer, Version.DRAFT_05);
		while (track.subscription.peek()?.priority !== 9) await track.subscription.changed();

		release();
		const opened = pair.client.incomingUnidirectionalStreams.getReader();
		if ((await opened.read()).done) throw new Error("publisher never opened the group stream");
		opened.releaseLock();

		await clock.advance(Duration.ZERO);

		expect(pair.server.sendStreams.uni[0]?.sendOrder).toBe(sendOrder({ priority: 9 }));
	} finally {
		group.finish();
		publisher.close();
		client.close();
	}
});

// A stalled track piles up open groups, and the signals leak guard throws at 100 subscribers
// on one signal, so the subscription is ranked by a single listener rather than one per group.
test("lite draft-05: many concurrent groups share one subscription listener", async () => {
	using clock = manual();
	const count = 120;
	const pair = createMockTransportPair(ALPN_05);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomHop(), origin.consume());

	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null });

	const client = await Stream.open(pair.client, { version: Version.DRAFT_05 });
	const server = await Stream.accept(pair.server, Version.DRAFT_05);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = replaySubscribe({
		id: 0n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 1,
	});
	void publisher.runSubscribe(msg, server);

	// Leave every group open, so all of their streams are in flight at once.
	const groups: GroupProducer[] = [];
	const opened = pair.client.incomingUnidirectionalStreams.getReader();

	try {
		for (let sequence = 0n; sequence < BigInt(count); sequence++) {
			const group = track.createGroup(sequence);
			groups.push(group);
			group.writeFrame(textFrame("hello"));
			await servingNextGroup(opened);
		}
		opened.releaseLock();

		expect(pair.server.sendStreams.uni.length).toBe(count);

		await replayUpdate({ priority: 9 }).encode(client.writer, Version.DRAFT_05);
		while (track.subscription.peek()?.priority !== 9) await track.subscription.changed();

		// Newest-first, so the last group opened is at position 0 and the first is at the back.
		const expected = groups.map((group) =>
			sendOrder({ priority: 9, position: count - 1 - Number(group.sequence) }),
		);
		await clock.advance(Duration.ZERO);

		expect(pair.server.sendStreams.uni.map((stream) => stream.sendOrder)).toEqual(expected);
	} finally {
		for (const group of groups) group.finish();
		publisher.close();
		client.close();
	}
});

// A send order only schedules the local end, so the subscriber's FETCH stream ranks its own
// request; without this the response competes with the group streams at the default order.
test("lite draft-05: the fetch response ranks the publisher's own writes", async () => {
	const pair = createMockTransportPair(ALPN_05);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomHop(), origin.consume());

	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const group = track.createGroup(7n);
	group.writeFrame(textFrame("hello"));
	group.finish();

	const client = await Stream.open(pair.client, { version: Version.DRAFT_05 });

	// Accept by hand rather than via Stream.accept, so the test keeps the writable the
	// publisher ranks (a real WebTransportSendStream takes the same assignment).
	const incoming = pair.server.incomingBidirectionalStreams.getReader();
	const accepted = await incoming.read();
	incoming.releaseLock();
	if (accepted.done) throw new Error("publisher never saw the fetch stream");
	const server = new Stream({ ...accepted.value, version: Version.DRAFT_05 });

	const msg = new Fetch({ broadcast: Path.from("test"), track: "video", priority: 3, group: 7n });
	try {
		await publisher.runFetch(msg, server);

		expect((accepted.value.writable as { sendOrder?: number }).sendOrder).toBe(sendOrder({ priority: 3 }));
	} finally {
		publisher.close();
		client.close();
	}
});

test.each([0, 5, 6, 7])("a fetched cached tail keeps absolute indices from %s", async (startFrame) => {
	const version = Version.DRAFT_07;
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const group = hooks.receiveGroup(track, { sequence: 0n, start: 5 });
	for (const value of ["a", "b"]) group.writeFrame(textFrame(value));
	group.finish();
	const client = await Stream.open(pair.client, { version });
	const server = await Stream.accept(pair.server, version);
	if (!server) throw new Error("publisher never saw the fetch stream");
	try {
		await publisher.runFetch(
			new Fetch({
				broadcast: Path.from("test"),
				track: "video",
				priority: 0,
				group: 0n,
				startFrame,
			}),
			server,
		);
		if (startFrame === 0) {
			await expect(client.reader.done()).rejects.toThrow();
		} else {
			const payloads: string[] = [];
			while (!(await client.reader.done())) {
				// Lite-07 leads with the size plus one, then the zigzag timestamp delta.
				const size = (await client.reader.u53()) - 1;
				await client.reader.u62();
				payloads.push(new TextDecoder().decode(await client.reader.read(size)));
			}
			expect(payloads).toEqual(["a", "b"].slice(startFrame - 5));
		}
	} finally {
		publisher.close();
		client.close();
		broadcast.close();
		origin.close();
	}
});

// An unchecked track's frames go out with their own timestamps, which may be moq-transport
// arrival times, rather than their send time.
test("lite draft-06: an unchecked track's fetched frames carry their own timestamp", async () => {
	const pair = createMockTransportPair(ALPN_06);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_06, randomHop(), origin.consume());

	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MICRO, unchecked: true });

	const group = track.createGroup(0n);
	group.writeFrame(textFrame("hello", Timestamp.fromMillis(36_000_000)));
	group.finish();

	const client = await Stream.open(pair.client, { version: Version.DRAFT_06 });
	const server = await Stream.accept(pair.server, Version.DRAFT_06);
	if (!server) throw new Error("publisher never saw the fetch stream");

	const msg = new Fetch({ broadcast: Path.from("test"), track: "video", priority: 0, group: 0n });
	try {
		await publisher.runFetch(msg, server);

		// The first frame's zigzag delta is against 0, in the microseconds TRACK_INFO declares.
		expect(await client.reader.u62()).toBe(36_000_000_000n << 1n);
	} finally {
		publisher.close();
		client.close();
	}
});

// How long a served-subscription test waits on its manual clock for the next group stream
// before calling the publisher idle.
const IDLE_MS = Duration.millis(500);

// One macrotask turn, which drains every microtask queued behind it. Signal notifications
// are coalesced per microtask, so this is what lets a just-written group reach the serving
// loop's armed `recvGroup` without a test guessing at a delay.
const flush = () => yieldNow();

function lastGroups(ranges: ReturnType<typeof spyOn>) {
	return ranges.mock.calls.at(-1)?.[0];
}

// Wraps a writable so its writes park until `release()`, giving a test a window inside
// whatever the publisher is writing while its other loops keep running. `parked` settles on
// the first write attempt, held or not. Passing an error to `release` fails the held write
// instead, which is how a test drives the publisher's error teardown from a known point.
function gateWrites(target: WritableStream<Uint8Array>, hold: boolean) {
	const writer = target.getWriter();

	let release!: (err?: Error) => void;
	const released = new Promise<void>((resolve, reject) => {
		release = (err) => (err ? reject(err) : resolve());
	});
	// Nobody awaits this until a write parks on it, so a rejection would look unhandled.
	released.catch(() => void 0);
	if (!hold) release();

	let parking!: () => void;
	const parked = new Promise<void>((resolve) => {
		parking = resolve;
	});

	const writable = new WritableStream<Uint8Array>({
		async write(chunk) {
			parking();
			await released;
			await writer.write(chunk);
		},
		close: () => writer.close(),
		abort: (err) => writer.abort(err),
	});

	return { writable, parked, release };
}

// Opens a served subscription and returns the machinery to write groups and observe
// which of them the publisher put on the wire (each group gets its own uni stream).
//
// `gated` holds the publisher's subscribe-stream writes until `release()`, parking the
// serving loop mid-iteration so a test can drive the concurrent SUBSCRIBE_UPDATE loop
// against a group the loop is already holding.
async function servedSubscription(
	options: {
		ranges?: Subscribe["ranges"];
		startGroup?: bigint;
		startFrame?: number;
		endGroup?: bigint;
		endFrame?: number;
		gated?: boolean;
		maxAge?: Duration;
		// Frame payloads written into every served group. Frame bounds need draft-06.
		frames?: string[];
		version?: Version;
		maxDatagramSize?: number;
		// Answer the subscription with a front's broadcast, as an application forwarding a
		// routed one does, so the track reads through the front's serving route.
		routed?: boolean;
		// Groups the track holds before the SUBSCRIBE arrives, stamped by sequence.
		prefill?: bigint[];
		// A newer largest group the track's source reported before the SUBSCRIBE arrives.
		reported?: bigint;
		// Leave the track unresolved until `accept`, as an on-demand track the SUBSCRIBE beats.
		pending?: boolean;
	} = {},
) {
	const version = options.version ?? Version.DRAFT_05;
	const frames = options.frames ?? ["hello"];
	const pair = createMockTransportPair(version === Version.DRAFT_06 ? ALPN_06 : ALPN_05);
	if (options.maxDatagramSize !== undefined)
		Object.defineProperty(pair.server.datagrams, "maxDatagramSize", { value: options.maxDatagramSize });
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());

	let broadcast: BroadcastProducer;
	let routed: { front: Front; dynamic: { close(): void } } | undefined;
	if (options.routed) {
		broadcast = new BroadcastProducer();
		const front = new Front();
		front.serve(broadcast.consume());
		const dynamic = origin.claim(Path.from("test"));
		void (async () => {
			for await (const request of dynamic.requested()) request.serve(front.consumer);
		})();
		routed = { front, dynamic };
	} else {
		broadcast = publish(origin, Path.from("test"));
	}
	// Unset is no publisher limit: these fixtures serve superseded groups.
	const info = { timescale: Timescale.MILLI, maxAge: options.maxAge ?? null };
	let track = options.pending ? undefined : broadcast.createTrack("video", info);
	for (const sequence of options.prefill ?? []) {
		if (!track) throw new Error("a pending track holds nothing to prefill");
		const group = track.createGroup(sequence);
		for (const frame of frames) group.writeFrame(textFrame(frame, Timestamp.fromMillis(Number(sequence))));
		group.finish();
	}
	if (options.reported !== undefined) {
		if (!track) throw new Error("a pending track has no source to report");
		hooks.reportedGroup(track, options.reported);
	}
	// Pulled before the SUBSCRIBE arrives: a broadcast nobody pulls refuses an unknown track.
	const request = options.pending ? wireOf(broadcast).requested() : undefined;

	const client = await Stream.open(pair.client, { version: version });

	// Accept by hand rather than via Stream.accept, so the gate sits between the publisher
	// and the wire.
	const incoming = pair.server.incomingBidirectionalStreams.getReader();
	const accepted = await incoming.read();
	incoming.releaseLock();
	if (accepted.done) throw new Error("publisher never accepted the subscribe stream");

	const gate = gateWrites(accepted.value.writable, options.gated ?? false);
	const server = new Stream({ readable: accepted.value.readable, writable: gate.writable, version: version });

	const msg = replaySubscribe({
		id: 0n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 0,
		startGroup: options.startGroup,
		startFrame: options.startFrame,
		endGroup: options.endGroup,
		endFrame: options.endFrame,
	});
	// Kept so a test can wait for the whole subscription to unwind, decoder included, rather
	// than only for what reached the wire. It reports failures by tearing down, never by
	// rejecting.
	msg.ranges = options.ranges;
	const serving = publisher.runSubscribe(msg, server);

	const opened = pair.client.incomingUnidirectionalStreams.getReader();

	const resolved = () => {
		if (!track) throw new Error("the track is still pending");
		return track;
	};

	return {
		client,
		// The publisher's end of the session.
		quic: pair.server,
		datagrams: pair.client.datagrams.readable,
		get track() {
			return resolved();
		},
		serving,
		parked: gate.parked,
		release: gate.release,
		// Resolve a pending track with the publisher's request for it, answering with `held`
		// and, when `reported`, an upstream's report of a newer largest group.
		async accept(held: bigint[] = [], reported?: bigint) {
			const pulled = await request;
			if (!pulled) throw new Error("the publisher never requested the track");
			const staged = hooks.stageRequest(pulled, info);
			if (reported !== undefined) hooks.reportedGroup(staged.producer, reported);
			for (const sequence of held) {
				const group = staged.producer.createGroup(sequence);
				for (const frame of frames) group.writeFrame(textFrame(frame, Timestamp.fromMillis(Number(sequence))));
				group.finish();
			}
			track = staged.commit();
		},
		// Stamped by sequence, so a group served out of order still starts in order.
		serve(sequence: bigint) {
			const group = resolved().createGroup(sequence);
			for (const frame of frames) group.writeFrame(textFrame(frame, Timestamp.fromMillis(Number(sequence))));
			group.finish();
		},
		// Makes the next group stream withhold its FIN from the client, the way a stream the
		// browser never finishes sending looks; resolves with the reason the publisher resets it.
		stall(): Promise<unknown> {
			const open = pair.server.createUnidirectionalStream.bind(pair.server);
			return new Promise((resolve) => {
				pair.server.createUnidirectionalStream = async (options) => {
					pair.server.createUnidirectionalStream = open;
					const writer = (await open(options)).getWriter();
					return new WritableStream<Uint8Array>({
						start(controller) {
							// A WebTransport send stream resets on the controller's signal; the
							// sink's abort would wait for the close that never settles.
							controller.signal.addEventListener("abort", () => {
								resolve(controller.signal.reason);
								void writer.abort(controller.signal.reason);
							});
						},
						write: (chunk) => writer.write(chunk),
						close: () => new Promise(() => undefined),
					});
				};
			});
		},
		// The sequence of the next group stream the publisher opened, from its header alone,
		// for a stream whose end never comes.
		async servedHeader(): Promise<bigint | undefined> {
			const next = await opened.read();
			if (next.done) return undefined;
			const reader = new Reader(next.value, undefined, version);
			await reader.u53(); // stream type
			return (await GroupMessage.decode(reader, version)).sequence;
		},
		// Whether the publisher opens another group stream before `clock` passes IDLE_MS. The
		// read stays queued, so it takes the next stream: call this last.
		opensGroup(clock: Manual): Promise<boolean> {
			return settlesWithin(opened.read(), clock, IDLE_MS);
		},
		// The next group stream the publisher opened, drained, or undefined once the session ends.
		async servedGroup(): Promise<Served | undefined> {
			const next = await opened.read();
			if (next.done) return undefined;

			const reader = new Reader(next.value, undefined, version);
			await reader.u53(); // stream type
			const header = await GroupMessage.decode(reader, version);

			const payloads: string[] = [];
			while (!(await reader.done())) {
				// Each frame is a zigzag timestamp delta, then a length-prefixed payload. Lite-07
				// leads with the size plus one instead.
				const lengthFirst = hasLengthFirst(version);
				const length = lengthFirst ? (await reader.u53()) - 1 : undefined;
				await reader.u62();
				const size = length ?? (await reader.u53());
				payloads.push(new TextDecoder().decode(await reader.read(size)));
			}
			return { sequence: header.sequence, frameStart: header.frameStart, payloads };
		},
		// The sequence alone, for tests that only care which groups reached the wire.
		async servedSequence(): Promise<bigint | undefined> {
			return (await this.servedGroup())?.sequence;
		},
		async close() {
			// Settles any pending read as well as dropping the stream.
			await opened.cancel();
			publisher.close();
			client.close();
			routed?.dynamic.close();
			routed?.front.consumer.close();
		},
	};
}

// SUBSCRIBE_START tells the subscriber where each range's start landed, which a relative or time
// start leaves it no way to work out from the groups it receives. Mirrors the Rust
// `lite07_start_names_each_resolved_range_start`.
test("lite-07 SUBSCRIBE_START names each resolved range start", async () => {
	const version = Version.DRAFT_07;
	const sub = await servedSubscription({
		version,
		prefill: [0n, 1n, 2n, 3n, 4n],
		maxAge: Duration.millis(3_600_000),
		ranges: [
			{ start: { relative: 1n } },
			// Group 1 starts at 1 ms.
			{ start: { time: 1 }, end: { absolute: 2n } },
			// Empty, so it starts nowhere.
			{ start: { absolute: 3n }, end: { absolute: 3n } },
			// Empty only once its start resolves to group 4.
			{ start: { relative: 0n }, end: { absolute: 2n } },
		],
	});
	try {
		const resp = await decodeSubscribeResponse(sub.client.reader, version);
		if (!("start" in resp)) throw new Error("expected SUBSCRIBE_START");
		expect(resp.start.starts).toEqual([3n, 1n, undefined, undefined]);
	} finally {
		await sub.close();
	}
});

// Lite-07: a group whose stream the session reset to make room continues on a new stream from the
// reset one's Frame Start, even where the cache holds only the group's tail from there, rather than
// being dropped as OLD.
test("lite-07 continues a yielded cached tail from its Frame Start", async () => {
	const version = Version.DRAFT_07;
	const sub = await servedSubscription({ version, startGroup: 0n, startFrame: 5 });
	try {
		// A received tail: the cache holds the group from frame 5, where the subscription starts.
		const group = hooks.receiveGroup(sub.track, { sequence: 0n, start: 5 });
		group.writeFrame(textFrame("a", Timestamp.fromMillis(0)));
		expect(await sub.servedHeader()).toBe(0n);
		expect("start" in (await decodeSubscribeResponse(sub.client.reader, version))).toBe(true);

		// Unfinished, so its stream is still open when the session resets it to make room.
		const [open] = lines(sub.quic).streams.list();
		if (!open) throw new Error("the group stream is not open");
		open.preempt();
		group.writeFrame(textFrame("b", Timestamp.fromMillis(1)));
		group.finish();

		expect(await sub.servedGroup()).toEqual({ sequence: 0n, frameStart: 5, payloads: ["a", "b"] });

		// Responses arrive in order, so a drop naming the continued group would precede the end.
		sub.track.finish();
		const after = await decodeSubscribeResponse(sub.client.reader, version);
		expect(Object.keys(after)).toEqual(["finish"]);
	} finally {
		await sub.close();
	}
});

// Lite-07: a yielded group the publisher aborts before its continuation is dropped with the
// publisher's error, not as stale. Mirrors the Rust `lite07_yielded_group_aborted_drops_with_its_error`.
test("lite-07 drops a yielded group the publisher aborted with its error", async () => {
	const version = Version.DRAFT_07;
	const sub = await servedSubscription({ version, startGroup: 0n });
	try {
		const group = sub.track.createGroup(0n);
		group.writeFrame(textFrame("a", Timestamp.fromMillis(0)));
		expect(await sub.servedHeader()).toBe(0n);
		expect("start" in (await decodeSubscribeResponse(sub.client.reader, version))).toBe(true);

		const [open] = lines(sub.quic).streams.list();
		if (!open) throw new Error("the group stream is not open");
		open.preempt();
		group.abort(new StreamError(StreamCode(70), { message: "cut" }));

		const resp = await decodeSubscribeResponse(sub.client.reader, version);
		if (!("drop" in resp)) throw new Error("expected SUBSCRIBE_DROP");
		expect([resp.drop.start, resp.drop.end, resp.drop.error]).toEqual([0n, 1n, 70]);
	} finally {
		await sub.close();
	}
});

// A range an update adds learns where it starts from SUBSCRIBE_STARTED, and keeps that start however
// many later updates leave it live, rather than counting back from a newer latest group.
test("lite-07 SUBSCRIBE_STARTED reports an added range's start, which later updates keep", async () => {
	const version = Version.DRAFT_07;
	const sub = await servedSubscription({
		version,
		prefill: [0n, 1n, 2n, 3n, 4n],
		maxAge: Duration.millis(3_600_000),
		ranges: [{ start: { absolute: 0n }, end: { absolute: 1n } }],
	});
	const next = async () => {
		for (;;) {
			const resp = await decodeSubscribeResponse(sub.client.reader, version);
			if ("start" in resp || "started" in resp) return resp;
		}
	};
	try {
		const start = await next();
		if (!("start" in start)) throw new Error("expected SUBSCRIBE_OK");
		expect(start.start.starts).toEqual([0n]);

		// Two groups back from the latest, group 2, which takes Range ID 1.
		await replayUpdate({ priority: 0, ranges: [{ start: { relative: 2n } }] }).encode(sub.client.writer, version);
		const started = await next();
		if (!("started" in started)) throw new Error("expected SUBSCRIBE_STARTED");
		expect([started.started.id, started.started.start]).toEqual([1n, 2n]);

		// Newer groups arrive, and another update leaves the range live: it still starts at 2.
		for (const sequence of [5n, 6n, 7n]) sub.serve(sequence);
		await replayUpdate({ priority: 0, ranges: [{ start: { absolute: 9n } }] }).encode(sub.client.writer, version);
		const pinned = await next();
		if (!("started" in pinned)) throw new Error("expected SUBSCRIBE_STARTED");
		expect([pinned.started.id, pinned.started.start]).toEqual([2n, 9n]);
		const ranges = sub.track.subscription.peek()?.ranges ?? [];
		expect(ranges.some((range) => range.start?.absolute === 2n)).toBe(true);
		expect(ranges.some((range) => range.start?.relative !== undefined)).toBe(false);
	} finally {
		await sub.close();
	}
});

// A time start resolves once too: a newer group starting before its time doesn't move it once
// an update leaves the range live. Mirrors the Rust `lite07_an_added_time_start_keeps_its_start`.
test("lite-07 keeps where an added time start resolved", async () => {
	const version = Version.DRAFT_07;
	const sub = await servedSubscription({
		version,
		prefill: [0n, 1n, 2n, 3n, 4n],
		maxAge: Duration.millis(3_600_000),
		ranges: [{ start: { absolute: 0n }, end: { absolute: 1n } }],
	});
	const next = async () => {
		for (;;) {
			const resp = await decodeSubscribeResponse(sub.client.reader, version);
			if ("start" in resp || "started" in resp) return resp;
		}
	};
	try {
		const start = await next();
		if (!("start" in start)) throw new Error("expected SUBSCRIBE_OK");

		// 10 ms falls in group 4, the newest one starting at or before it.
		await replayUpdate({ priority: 0, ranges: [{ start: { time: 10 } }] }).encode(sub.client.writer, version);
		const started = await next();
		if (!("started" in started)) throw new Error("expected SUBSCRIBE_STARTED");
		expect([started.started.id, started.started.start]).toEqual([1n, 4n]);

		// Group 5 starts at 5 ms, before 10 ms, and another update leaves the range live: it
		// still starts at 4.
		sub.serve(5n);
		await replayUpdate({ priority: 0, ranges: [{ start: { absolute: 9n } }] }).encode(sub.client.writer, version);
		const pinned = await next();
		if (!("started" in pinned)) throw new Error("expected SUBSCRIBE_STARTED");
		expect(pinned.started.id).toBe(2n);
		const ranges = sub.track.subscription.peek()?.ranges ?? [];
		expect(ranges.some((range) => range.start?.absolute === 4n)).toBe(true);
		expect(ranges.some((range) => range.start?.time !== undefined)).toBe(false);
	} finally {
		await sub.close();
	}
});

// Group 0 included: a time start that resolved to group 0 names it, so a newer group starting
// before its time doesn't move it either.
test("lite-07 keeps an added time start that resolved to group 0", async () => {
	const version = Version.DRAFT_07;
	const sub = await servedSubscription({
		version,
		prefill: [0n],
		maxAge: Duration.millis(3_600_000),
		ranges: [{ start: { absolute: 0n }, end: { absolute: 1n } }],
	});
	const next = async () => {
		for (;;) {
			const resp = await decodeSubscribeResponse(sub.client.reader, version);
			if ("start" in resp || "started" in resp) return resp;
		}
	};
	try {
		if (!("start" in (await next()))) throw new Error("expected SUBSCRIBE_OK");

		// 10 ms falls in group 0, the only group.
		await replayUpdate({ priority: 0, ranges: [{ start: { time: 10 } }] }).encode(sub.client.writer, version);
		const started = await next();
		if (!("started" in started)) throw new Error("expected SUBSCRIBE_STARTED");
		expect([started.started.id, started.started.start]).toEqual([1n, 0n]);

		// Group 1 starts at 1 ms, before 10 ms; the range still starts at group 0.
		sub.serve(1n);
		await replayUpdate({ priority: 0, ranges: [{ start: { absolute: 9n } }] }).encode(sub.client.writer, version);
		if (!("started" in (await next()))) throw new Error("expected SUBSCRIBE_STARTED");
		const ranges = sub.track.subscription.peek()?.ranges ?? [];
		expect(ranges.some((range) => range.start?.time !== undefined)).toBe(false);
		expect(ranges.some((range) => range.start?.absolute === 0n && range.end === undefined)).toBe(true);
	} finally {
		await sub.close();
	}
});

// An edit naming a Range ID the subscription never assigned violates the protocol.
test("lite-07 refuses an update retiring an unknown Range ID", async () => {
	const version = Version.DRAFT_07;
	const sub = await servedSubscription({ version, prefill: [0n], ranges: [{ start: { absolute: 0n } }] });
	try {
		await replayUpdate({ priority: 0, ranges: [], remove: [1n] }).encode(sub.client.writer, version);
		await sub.serving;
		await expect(sub.client.reader.closed).rejects.toThrow();
	} finally {
		await sub.close();
	}
});

// A bounded window that resolves empty has no group to answer it, so the publisher answers it
// at once. The stream stays open for an update until the subscriber FINs, and the publisher
// FINs in reply, even when the FIN lands while the reply's write yields to the host.
test("lite-07 answers a bounded window that resolves empty and FINs in reply", async () => {
	const version = Version.DRAFT_07;
	const yielding = spyOn(writeBudget, "poll").mockImplementation(() => yieldNow());
	try {
		const sub = await servedSubscription({
			version,
			prefill: [0n, 1n, 2n, 3n, 4n, 5n],
			maxAge: Duration.millis(3_600_000),
			ranges: [{ start: { relative: 0n }, end: { absolute: 3n } }],
		});
		try {
			const resp = await decodeSubscribeResponse(sub.client.reader, version);
			if (!("start" in resp)) throw new Error("expected SUBSCRIBE_START");
			expect(resp.start.starts).toEqual([undefined]);
			sub.client.writer.close();
			expect(await decodeSubscribeResponseMaybe(sub.client.reader, version)).toBeUndefined();
		} finally {
			await sub.close();
		}
	} finally {
		yielding.mockRestore();
	}
});

// A time anchor waits for the track's timescale. On an on-demand track that lookup is the
// subscription's own request, so the track it answers is the one served.
test("lite-07 serves a time range from the on-demand track its lookup resolved", async () => {
	const sub = await servedSubscription({
		version: Version.DRAFT_07,
		pending: true,
		ranges: [{ start: { time: 0 } }],
	});
	try {
		await sub.accept();
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
	} finally {
		await sub.close();
	}
});

// A relay can ingest back-to-back groups micro-reordered (the upstream leg sends
// newest-first). The older group is cached and in demand, so serving must still
// deliver it; a sequence cursor would skip it permanently.
test("lite draft-05: a late-arriving older group is still served", async () => {
	const sub = await servedSubscription({ startGroup: 1n });
	// A received copy: a local publisher's gap is skipped at once, so its late group is not.
	hooks.receiveTrack(sub.track);
	try {
		// Serve one at a time so arrival order at the publisher is the order given.
		sub.serve(1n);
		expect(await sub.servedSequence()).toBe(1n);
		sub.serve(3n);
		expect(await sub.servedSequence()).toBe(3n);

		// Group 2 lands after group 3 was already served.
		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);
	} finally {
		await sub.close();
	}
});

// The SUBSCRIBE_DROP ranges a subscribe stream carried up to its FIN.
async function drops(reader: Reader, version: Version): Promise<[bigint, bigint][]> {
	const ranges: [bigint, bigint][] = [];
	for (;;) {
		const resp = await decodeSubscribeResponseMaybe(reader, version);
		if (!resp) return ranges;
		if ("drop" in resp) ranges.push([resp.drop.start, resp.drop.end]);
	}
}

// A cut group is dropped as soon as the publisher gives it up, and the end sweep does not
// name it again.
test("lite draft-06: a cut group is dropped early, once", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_06, startGroup: 0n });
	try {
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
		expect("start" in (await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_06))).toBe(true);

		const cut = sub.track.createGroup(1n);
		cut.abort(new Error("cut"));
		const resp = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_06);
		if (!("drop" in resp)) throw new Error("expected SUBSCRIBE_DROP");
		expect([resp.drop.start, resp.drop.end]).toEqual([1n, 2n]);

		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);
		sub.track.finish();
		sub.track.close();
		expect(await drops(sub.client.reader, Version.DRAFT_06)).toEqual([]);
	} finally {
		await sub.close();
	}
});

// Lite-07 names why with a stream error code: the cut's own, and NOT_FOUND for what the end
// sweep finds never served. Earlier versions carry an application code there, so 0.
for (const version of [Version.DRAFT_06, Version.DRAFT_07]) {
	test(`lite ${version}: a SUBSCRIBE_DROP carries ${version === Version.DRAFT_07 ? "the stream error code" : "no code"}`, async () => {
		const sub = await servedSubscription({ version, startGroup: 0n });
		try {
			sub.serve(0n);
			expect(await sub.servedSequence()).toBe(0n);
			expect("start" in (await decodeSubscribeResponse(sub.client.reader, version))).toBe(true);

			const cut = sub.track.createGroup(1n);
			cut.abort(new StreamError(StreamCode(70), { message: "cut" }));
			const resp = await decodeSubscribeResponse(sub.client.reader, version);
			if (!("drop" in resp)) throw new Error("expected SUBSCRIBE_DROP");
			expect([resp.drop.start, resp.drop.end, resp.drop.error]).toEqual([
				1n,
				2n,
				version === Version.DRAFT_07 ? 70 : 0,
			]);
		} finally {
			await sub.close();
		}
	});

	test(`lite ${version}: a routed track forwards its serving route's drop`, async () => {
		const sub = await servedSubscription({ version, startGroup: 0n, routed: true });
		try {
			sub.serve(0n);
			expect(await sub.servedSequence()).toBe(0n);
			expect("start" in (await decodeSubscribeResponse(sub.client.reader, version))).toBe(true);

			hooks.dropTrackGroups(sub.track, 1n, 3n, { error: new StreamError(StreamCode(70), { message: "cut" }) });
			const resp = await decodeSubscribeResponse(sub.client.reader, version);
			if (!("drop" in resp)) throw new Error("expected SUBSCRIBE_DROP");
			expect([resp.drop.start, resp.drop.end, resp.drop.error]).toEqual([
				1n,
				3n,
				version === Version.DRAFT_07 ? 70 : 0,
			]);
		} finally {
			await sub.close();
		}
	});

	// A skip replaces a drop, so a relay forwards it over a drop it already forwarded; lite-06,
	// which names a skip as a drop, sends nothing more. Mirrors Rust.
	test(`lite ${version}: a skip goes out over a forwarded drop`, async () => {
		const sub = await servedSubscription({ version, startGroup: 0n, routed: true });
		try {
			sub.serve(0n);
			expect(await sub.servedSequence()).toBe(0n);
			expect("start" in (await decodeSubscribeResponse(sub.client.reader, version))).toBe(true);

			hooks.dropTrackGroups(sub.track, 1n, 3n, { error: new StreamError(StreamCode(70), { message: "cut" }) });
			const resp = await decodeSubscribeResponse(sub.client.reader, version);
			if (!("drop" in resp)) throw new Error("expected SUBSCRIBE_DROP");
			expect([resp.drop.start, resp.drop.end]).toEqual([1n, 3n]);

			sub.track.skipGroups(1n, 3n);
			if (version === Version.DRAFT_07) {
				const skip = await decodeSubscribeResponse(sub.client.reader, version);
				expect(skip).toMatchObject({ skip: { start: 1n, end: 3n } });
			}
			sub.track.finish();
			sub.track.close();
			const holes: unknown[] = [];
			for (;;) {
				const next = await decodeSubscribeResponseMaybe(sub.client.reader, version);
				if (!next) break;
				if ("drop" in next) holes.push(["drop", next.drop.start, next.drop.end]);
				if ("skip" in next) holes.push(["skip", next.skip.start, next.skip.end]);
				if ("finish" in next && keepsSubscribeOpen(version)) break;
			}
			expect(holes).toEqual([]);
		} finally {
			await sub.close();
		}
	});

	// A lite-07 stale (OLD) drop is no final verdict, so a group handed out after it is served, as
	// when a front switches to a route that delivers what the old route reported stale, before the
	// update that reopens it. Earlier versions' drops are final. Mirrors Rust.
	test(`lite ${version}: a group ${version === Version.DRAFT_07 ? "replaces" : "stays dropped after"} a stale drop`, async () => {
		const sub = await servedSubscription({ version, startGroup: 0n });
		try {
			sub.serve(0n);
			expect(await sub.servedSequence()).toBe(0n);
			expect("start" in (await decodeSubscribeResponse(sub.client.reader, version))).toBe(true);

			hooks.dropTrackGroups(sub.track, 1n, 2n, staleHole(60_000));
			const resp = await decodeSubscribeResponse(sub.client.reader, version);
			if (!("drop" in resp)) throw new Error("expected SUBSCRIBE_DROP");
			expect([resp.drop.start, resp.drop.end]).toEqual([1n, 2n]);

			sub.serve(1n);
			sub.serve(2n);
			const next = await sub.servedSequence();
			expect(next).toBe(version === Version.DRAFT_07 ? 1n : 2n);
		} finally {
			await sub.close();
		}
	});

	for (const declared of [false, true]) {
		const named = declared && version === Version.DRAFT_07 ? "in SUBSCRIBE_SKIP" : "in SUBSCRIBE_DROP";
		test(`lite ${version}: the end sweep names a ${declared ? "declared skip" : "gap never declared"} ${named}`, async () => {
			const sub = await servedSubscription({ version, startGroup: 5n });
			try {
				sub.serve(5n);
				expect(await sub.servedSequence()).toBe(5n);
				await replayUpdate({ priority: 0, startGroup: 2n, ...restate(version) }).encode(
					sub.client.writer,
					version,
				);
				await flush();
				// Only a declared skip says groups 2 through 4 were never produced; a gap alone
				// might have held them, so it is not found.
				if (declared) sub.track.skipGroups(2n, 6n);
				sub.track.finish();
				sub.track.close();
				const codes: number[] = [];
				const skips: bigint[][] = [];
				for (;;) {
					const resp = await decodeSubscribeResponseMaybe(sub.client.reader, version);
					if (!resp) break;
					if ("drop" in resp) codes.push(resp.drop.error);
					if ("skip" in resp) skips.push([resp.skip.start, resp.skip.end]);
					// Lite-07 keeps the stream open past SUBSCRIBE_FINISH, after the sweep's drops.
					if ("finish" in resp && keepsSubscribeOpen(version)) break;
				}
				if (version !== Version.DRAFT_07) {
					expect([codes, skips]).toEqual([[0], []]);
				} else if (declared) {
					expect([codes, skips]).toEqual([[], [[2n, 5n]]]);
				} else {
					expect([codes, skips]).toEqual([[StreamCode.NotFound], []]);
				}
			} finally {
				await sub.close();
			}
		});
	}
}

// A huge declared skip goes out as one SUBSCRIBE_SKIP, at a cost independent of its length.
// Mirrors Rust.
test("lite-07 skips a huge run at once", async () => {
	const FAR = 1n << 61n;
	const sub = await servedSubscription({ version: Version.DRAFT_07, startGroup: 0n });
	try {
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
		sub.track.skipGroups(1n, FAR);
		sub.track.finish();
		sub.track.close();
		const codes: number[] = [];
		const skips: bigint[][] = [];
		for (;;) {
			const resp = await decodeSubscribeResponseMaybe(sub.client.reader, Version.DRAFT_07);
			if (!resp) break;
			if ("drop" in resp) codes.push(resp.drop.error);
			if ("skip" in resp) skips.push([resp.skip.start, resp.skip.end]);
			if ("finish" in resp) break;
		}
		expect([codes, skips]).toEqual([[], [[1n, FAR]]]);
	} finally {
		await sub.close();
	}
});

// A cut this subscription already dropped stays dropped, as in Rust: the subscriber accounted
// for it, so a group the publisher re-creates under it afterwards is not served.
test("lite draft-06: a reported cut stays dropped when re-created", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_06, startGroup: 0n });
	try {
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
		expect("start" in (await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_06))).toBe(true);

		const cut = sub.track.createGroup(1n);
		cut.abort(new Error("cut"));
		const resp = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_06);
		if (!("drop" in resp)) throw new Error("expected SUBSCRIBE_DROP");
		expect([resp.drop.start, resp.drop.end]).toEqual([1n, 2n]);

		sub.serve(1n);
		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);
		sub.track.finish();
		sub.track.close();
		expect(await drops(sub.client.reader, Version.DRAFT_06)).toEqual([]);
	} finally {
		await sub.close();
	}
});

// A group stream that never completes after its FIN still holds the connection. Like the
// Rust `GroupServe`, the publisher waits on it only while the age budget keeps the group:
// the newest group is never stale, and once its successor arrived a full budget ago the
// stream is reset with `Old` and the group dropped.
test("lite draft-06: a group stream that never completes is reset once the group goes stale", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const budget = 1_000;
	// The track's max age caps the subscription's budget.
	const sub = await servedSubscription({
		version: Version.DRAFT_06,
		startGroup: 0n,
		maxAge: Duration.millis(budget),
	});
	try {
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
		expect("start" in (await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_06))).toBe(true);

		let reason: unknown;
		const reset = sub.stall().then((value) => {
			reason = value;
		});
		sub.serve(1n);
		expect(await sub.servedHeader()).toBe(1n);
		await flush();
		await clock.advance(Duration.millis(budget * 10));
		expect(reason).toBeUndefined();

		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);
		await clock.advance(Duration.millis(budget - 1));
		expect(reason).toBeUndefined();
		await clock.advance(Duration.millis(1));
		await reset;
		expect(toStreamCode(fromTransport(reason))).toBe(StreamCode.Old);
		const resp = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_06);
		if (!("drop" in resp)) throw new Error("expected SUBSCRIBE_DROP");
		expect([resp.drop.start, resp.drop.end]).toEqual([1n, 2n]);
	} finally {
		await sub.close();
		installed.close();
	}
});

// Lite-07 reopens every stale drop on an update, including one for a stream the publisher reset
// as stale: a group still stale under the new terms is dropped again under the new update's
// number, so the subscriber settles at once. Mirrors the Rust
// `an_update_reoffers_a_cached_expired_stream_on_lite07`.
test("lite draft-07: an update drops an expired stream again while it is still stale", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const budget = 1_000;
	const sub = await servedSubscription({
		version: Version.DRAFT_07,
		startGroup: 0n,
		maxAge: Duration.millis(budget),
	});
	try {
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
		expect("start" in (await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07))).toBe(true);

		const reset = sub.stall();
		sub.serve(1n);
		expect(await sub.servedHeader()).toBe(1n);
		await flush();
		await clock.advance(Duration.millis(budget * 10));
		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);
		await clock.advance(Duration.millis(budget));
		expect(toStreamCode(fromTransport(await reset))).toBe(StreamCode.Old);
		const first = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07);
		if (!("drop" in first)) throw new Error("expected SUBSCRIBE_DROP");
		expect([first.drop.start, first.drop.end, first.drop.update]).toEqual([1n, 2n, 0n]);

		// The track's max age still caps the budget, so group 1 is still stale.
		await replayUpdate({ priority: 1, startGroup: 0n, update: 1n, ...restate(Version.DRAFT_07) }).encode(
			sub.client.writer,
			Version.DRAFT_07,
		);
		const again = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07);
		if (!("drop" in again)) throw new Error("expected SUBSCRIBE_DROP");
		expect([again.drop.start, again.drop.end, again.drop.update]).toEqual([1n, 2n, 1n]);
	} finally {
		await sub.close();
		installed.close();
	}
});

// The drops and skips on a lite-07 subscribe stream up to its SUBSCRIBE_FINISH, with the update each
// drop names.
async function holesToEnd(reader: Reader): Promise<unknown[]> {
	const holes: unknown[] = [];
	for (;;) {
		const next = await decodeSubscribeResponseMaybe(reader, Version.DRAFT_07);
		if (!next) return holes;
		if ("drop" in next) holes.push(["drop", next.drop.start, next.drop.end, next.drop.update]);
		if ("skip" in next) holes.push(["skip", next.skip.start, next.skip.end]);
		if ("finish" in next) return holes;
	}
}

// A group that wins over a lite-07 stale drop already sent retires it: the next update neither
// sends the drop again nor reopens the served group, and the end names nothing for it. Here a
// relay's upstream reported group 1 stale, then delivered it, as a front that switched routes
// does. Mirrors the Rust `a_group_served_over_a_stale_drop_retires_it`.
test("lite draft-07: a group served over a stale drop retires it", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_07, startGroup: 0n });
	try {
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
		expect("start" in (await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07))).toBe(true);

		hooks.dropTrackGroups(sub.track, 1n, 2n, staleHole(60_000));
		const resp = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07);
		if (!("drop" in resp)) throw new Error("expected SUBSCRIBE_DROP");
		expect([resp.drop.start, resp.drop.end]).toEqual([1n, 2n]);
		sub.serve(1n);
		expect(await sub.servedSequence()).toBe(1n);

		await replayUpdate({ priority: 1, startGroup: 0n, update: 1n, ...restate(Version.DRAFT_07) }).encode(
			sub.client.writer,
			Version.DRAFT_07,
		);
		await flush();
		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);
		sub.track.finish();
		sub.track.close();
		// No second OLD, and no NOT_FOUND at the end.
		expect(await holesToEnd(sub.client.reader)).toEqual([]);
	} finally {
		await sub.close();
	}
});

// A lite-07 stream that expired as stale is a stale drop like any other: a final drop the reader
// reaches for it later replaces it, and no update drops it again then. Mirrors the Rust
// `a_final_drop_replaces_an_expired_stream_on_lite07`.
test("lite draft-07: a final drop replaces an expired stream", async () => {
	const clock = new Manual();
	const installed = clock.install();
	const budget = 1_000;
	const sub = await servedSubscription({
		version: Version.DRAFT_07,
		startGroup: 0n,
		maxAge: Duration.millis(budget),
	});
	try {
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
		expect("start" in (await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07))).toBe(true);

		const reset = sub.stall();
		// Still arriving, as a relay's copy from upstream: the upstream's final drop ends it. One
		// held whole would stay, since a group stream wins over its drop.
		const arriving = sub.track.createGroup(1n);
		arriving.writeFrame(textFrame("hello", Timestamp.fromMillis(1)));
		expect(await sub.servedHeader()).toBe(1n);
		await flush();
		await clock.advance(Duration.millis(budget * 10));
		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);
		await clock.advance(Duration.millis(budget));
		expect(toStreamCode(fromTransport(await reset))).toBe(StreamCode.Old);
		const first = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07);
		if (!("drop" in first)) throw new Error("expected SUBSCRIBE_DROP");
		expect([first.drop.start, first.drop.end, first.drop.update]).toEqual([1n, 2n, 0n]);

		hooks.dropTrackGroups(sub.track, 1n, 2n, { error: new StreamError(StreamCode(70), { message: "cut" }) });
		const final = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07);
		if (!("drop" in final)) throw new Error("expected SUBSCRIBE_DROP");
		expect([final.drop.start, final.drop.end, final.drop.error]).toEqual([1n, 2n, 70]);

		await replayUpdate({ priority: 1, startGroup: 0n, update: 1n, ...restate(Version.DRAFT_07) }).encode(
			sub.client.writer,
			Version.DRAFT_07,
		);
		await flush();
		sub.track.finish();
		sub.track.close();
		expect(await holesToEnd(sub.client.reader)).toEqual([]);
	} finally {
		await sub.close();
		installed.close();
	}
});

// A queued re-offer that a later update drops again as stale counts as given up: the track's end
// leaves that stale drop standing, rather than replacing it with a final one. Mirrors the Rust
// `a_redropped_reoffer_stays_stale_at_the_end_on_lite07`.
test("lite draft-07: a re-offer dropped again before it opens stays stale at the end", async () => {
	const clock = new Manual();
	const installed = clock.install();
	// Well inside the cache window, so the expired group is still cached to re-offer.
	const budget = 1_000;
	const update = (update: bigint, maxDelay: number, writer: Writer) =>
		new SubscribeUpdate({
			priority: 1,
			startGroup: 0n,
			update,
			maxDelay,
			...restate(Version.DRAFT_07, update - 1n),
		}).encode(writer, Version.DRAFT_07);
	const sub = await servedSubscription({ version: Version.DRAFT_07, startGroup: 0n });
	try {
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
		expect("start" in (await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07))).toBe(true);
		await update(1n, budget, sub.client.writer);
		// The restated range resolves at once.
		expect("started" in (await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07))).toBe(true);

		const reset = sub.stall();
		sub.serve(1n);
		expect(await sub.servedHeader()).toBe(1n);
		await flush();
		await clock.advance(Duration.millis(budget * 10));
		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);
		await clock.advance(Duration.millis(budget));
		expect(toStreamCode(fromTransport(await reset))).toBe(StreamCode.Old);
		const first = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07);
		if (!("drop" in first)) throw new Error("expected SUBSCRIBE_DROP");
		expect([first.drop.start, first.drop.end, first.drop.update]).toEqual([1n, 2n, 1n]);

		// Update 2 widens the budget, so group 1 is re-offered; update 3 restores it. Both arrive in
		// one chunk, which the decoder reads without leaving the task, so update 3 is decoded
		// before the task boundary the publisher yields after an update, and so before any pop.
		const batch = await Writer.collect(Version.DRAFT_07, async (writer) => {
			await update(2n, budget * 100, writer);
			await update(3n, budget, writer);
		});
		await sub.client.writer.write(batch);
		await flush();
		sub.track.finish();
		sub.track.close();
		expect(await holesToEnd(sub.client.reader)).toEqual([["drop", 1n, 2n, 3n]]);
	} finally {
		await sub.close();
		installed.close();
	}
});

// A drop the reader gave up as final with OLD, as one relayed from a lite-06 upstream is, goes out as
// a stale drop on lite-07, which the subscriber reopens on its next update, so each update drops it
// again rather than leave it owed. Mirrors the Rust
// `a_final_old_drop_is_dropped_again_by_each_update_on_lite07`.
test("lite draft-07: a final OLD drop is dropped again by each update", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_07, startGroup: 0n });
	try {
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
		expect("start" in (await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07))).toBe(true);

		hooks.dropTrackGroups(sub.track, 1n, 2n, { error: new StreamError(StreamCode.Old) });
		const first = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_07);
		if (!("drop" in first)) throw new Error("expected SUBSCRIBE_DROP");
		expect([first.drop.start, first.drop.end, first.drop.update]).toEqual([1n, 2n, 0n]);

		for (const update of [1n, 2n]) {
			await replayUpdate({
				priority: 1,
				startGroup: 0n,
				update,
				...restate(Version.DRAFT_07, update - 1n),
			}).encode(sub.client.writer, Version.DRAFT_07);
			await flush();
		}
		sub.track.finish();
		sub.track.close();
		expect(await holesToEnd(sub.client.reader)).toEqual([
			["drop", 1n, 2n, 1n],
			["drop", 1n, 2n, 2n],
		]);
	} finally {
		await sub.close();
	}
});

// An update that lowers the start below SUBSCRIBE_START asks for those groups too, so the
// end sweep drops them, as the Rust publisher does.
test("lite draft-06: an update lowering the start is swept at the end", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_06, startGroup: 5n });
	try {
		sub.serve(5n);
		expect(await sub.servedSequence()).toBe(5n);
		await replayUpdate({ priority: 0, startGroup: 2n }).encode(sub.client.writer, Version.DRAFT_06);
		await flush();
		sub.track.finish();
		sub.track.close();
		expect(await drops(sub.client.reader, Version.DRAFT_06)).toEqual([[2n, 5n]]);
	} finally {
		await sub.close();
	}
});

// A raised floor sends no early drop: a later update may lower it again, and the
// subscriber cannot take back a drop for the groups that reopens.
test("lite draft-06: a raised floor is not dropped early", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_06, startGroup: 0n });
	try {
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
		await replayUpdate({ priority: 0, startGroup: 5n }).encode(sub.client.writer, Version.DRAFT_06);
		await flush();
		await replayUpdate({ priority: 0, startGroup: 1n }).encode(sub.client.writer, Version.DRAFT_06);
		await flush();
		for (const sequence of [1n, 2n, 3n, 4n]) {
			sub.serve(sequence);
			expect(await sub.servedSequence()).toBe(sequence);
		}
		sub.track.finish();
		sub.track.close();
		expect(await drops(sub.client.reader, Version.DRAFT_06)).toEqual([]);
	} finally {
		await sub.close();
	}
});

// An unfloored pre-06 subscribe joins at the publisher's start. SUBSCRIBE_START makes
// that group the floor: a straggler below it is dropped even though arrival-order
// serving would otherwise surface it.
test("lite draft-05: a straggler below the announced start group is not served", async () => {
	const sub = await servedSubscription();
	try {
		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);

		// The publisher announced group 2 as the resolved start.
		const resp = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_05);
		if (!("start" in resp)) throw new Error("expected SUBSCRIBE_START");
		expect(resp.start.starts[0]).toBe(2n);

		// A straggler below the announced start never reaches the wire: the next
		// stream the publisher opens is group 3's.
		sub.serve(1n);
		sub.serve(3n);
		expect(await sub.servedSequence()).toBe(3n);
	} finally {
		await sub.close();
	}
});

// An explicit floor stays where it was named. SUBSCRIBE_START still reports the first
// served group, and a later group at or above the floor is delivered.
test("lite draft-05: an explicit floor still serves a group below the first one", async () => {
	const sub = await servedSubscription({ startGroup: 0n });
	try {
		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);

		const resp = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_05);
		if (!("start" in resp)) throw new Error("expected SUBSCRIBE_START");
		expect(resp.start.starts[0]).toBe(2n);

		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
	} finally {
		await sub.close();
	}
});

// A relay receives the newest group first while its source still promises older groups.
// START must preserve that promise, bounded by the downstream floor and first served group.
for (const version of [Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07]) {
	for (const [source, expected] of [
		[0n, 2n],
		[3n, 3n],
		[9n, 6n],
	]) {
		test(`lite ${version}: descending delivery resolves source start ${source} at ${expected}`, async () => {
			const sub = await servedSubscription({ version, startGroup: 2n });
			try {
				hooks.receiveTrack(sub.track);
				hooks.startTrack(sub.track, { start: source, requested: 2n });
				sub.serve(6n);
				expect(await sub.servedSequence()).toBe(6n);
				const response = await decodeSubscribeResponse(sub.client.reader, version);
				if (!("start" in response)) throw new Error("expected SUBSCRIBE_START");
				expect(response.start.starts[0]).toBe(expected);
				if (expected < 6n) {
					sub.serve(expected);
					expect(await sub.servedSequence()).toBe(expected);
				}
			} finally {
				await sub.close();
			}
		});
	}
}

// A relay's group stream can beat the upstream START on its own stream. Resolving from that
// group would give up the backlog the START still promises, so the first group waits for it.
for (const version of [Version.DRAFT_06, Version.DRAFT_07]) {
	test(`lite ${version}: START waits for the upstream START before resolving`, async () => {
		const sub = await servedSubscription({ version, startGroup: 2n });
		try {
			hooks.receiveTrack(sub.track);
			hooks.startTrack(sub.track, { requested: 2n });
			sub.serve(6n);
			await flush();
			hooks.startTrack(sub.track, { start: 3n, requested: 2n });
			const response = await decodeSubscribeResponse(sub.client.reader, version);
			if (!("start" in response)) throw new Error("expected SUBSCRIBE_START");
			expect(response.start.starts[0]).toBe(3n);
			expect(await sub.servedSequence()).toBe(6n);
		} finally {
			await sub.close();
		}
	});

	// A track that closes never gets its START, so the held group must not wait for it.
	test(`lite ${version}: a closed source releases the START it was waiting for`, async () => {
		const sub = await servedSubscription({ version, startGroup: 2n });
		try {
			hooks.receiveTrack(sub.track);
			hooks.startTrack(sub.track, { requested: 2n });
			sub.serve(6n);
			await flush();
			sub.track.finish();
			sub.track.close();
			const response = await decodeSubscribeResponse(sub.client.reader, version);
			if (!("start" in response)) throw new Error("expected SUBSCRIBE_START");
			expect(await sub.servedSequence()).toBe(6n);
		} finally {
			await sub.close();
		}
	});
}

for (const skipped of ["failed", "stale"] as const) {
	test(`lite draft-06: START advances past a ${skipped} group from the source`, async () => {
		const sub = await servedSubscription({
			version: Version.DRAFT_06,
			startGroup: 5n,
			maxAge: skipped === "stale" ? Duration.millis(0) : undefined,
		});
		try {
			hooks.receiveTrack(sub.track);
			hooks.startTrack(sub.track, { start: 5n, requested: 5n });
			const group = sub.track.createGroup(5n);
			group.writeFrame(textFrame("old", Timestamp.fromMillis(5)));
			if (skipped === "failed") group.abort(new Error("cut"));
			else group.finish();
			if (skipped === "failed") hooks.dropTrackGroups(sub.track, 7n, 8n, { error: new Error("cut") });
			sub.serve(6n);
			const response = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_06);
			if (!("start" in response)) throw new Error("expected SUBSCRIBE_START");
			expect(response.start.starts[0]).toBe(6n);
			if (skipped === "failed") {
				// The failed group below the start is final, so it is named all the same.
				for (const run of [
					[5n, 6n],
					[7n, 8n],
				]) {
					const dropped = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_06);
					if (!("drop" in dropped)) throw new Error("expected SUBSCRIBE_DROP");
					expect([dropped.drop.start, dropped.drop.end]).toEqual(run);
				}
			}
			expect(await sub.servedSequence()).toBe(6n);
		} finally {
			await sub.close();
		}
	});
}

// Pre-06 an absent Group Start is the latest group. An update that drops a named floor
// re-pins there, so a group created below it afterwards is not served.
test("lite draft-05: an update dropping the floor re-pins to the latest group", async () => {
	const sub = await servedSubscription({ startGroup: 0n });
	try {
		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);

		await replayUpdate({ priority: 0 }).encode(sub.client.writer, Version.DRAFT_05);
		await flush();
		sub.serve(0n);
		sub.serve(3n);
		expect(await sub.servedSequence()).toBe(3n);
	} finally {
		await sub.close();
	}
});

// A SUBSCRIBE can beat its track resolving, as one a relay serves from a copy still fetching
// upstream. Groups the track holds once it does are older than the join, so an absent Group
// Start is still the latest group known then, including one the upstream reported.
// Routed, the join reads the front's serving copy.
test.each([
	[Version.DRAFT_03, undefined, false],
	[Version.DRAFT_04, undefined, false],
	[Version.DRAFT_04, 9n, false],
	[Version.DRAFT_04, 9n, true],
	[Version.DRAFT_05, undefined, false],
	[Version.DRAFT_05, undefined, true],
])(
	"lite %s (reported %s, routed %s): a subscribe that beats its track resolving joins at its newest group",
	async (version, reported, routed) => {
		using clock = manual();
		const sub = await servedSubscription({ version, pending: true, routed });
		try {
			await sub.accept([0n, 3n, 7n], reported);
			if (reported !== undefined) {
				// The SUBSCRIBE_OK follows the floor, so these arrive after the join took it.
				expect("ok" in (await decodeSubscribeResponse(sub.client.reader, version))).toBe(true);
				sub.serve(8n);
				sub.serve(9n);
			}
			expect(await sub.servedHeader()).toBe(reported ?? 7n);
			expect(await sub.opensGroup(clock)).toBe(false);
		} finally {
			await sub.close();
		}
	},
);

// A track that already holds groups when the SUBSCRIBE arrives still joins at a newer largest
// its source reported, as a cold one would.
test.each([Version.DRAFT_04, Version.DRAFT_05])(
	"lite %s: a join on a track already holding groups starts at a reported newer group",
	async (version) => {
		using clock = manual();
		const sub = await servedSubscription({ version, prefill: [0n, 3n], reported: 9n });
		try {
			if (version === Version.DRAFT_04) {
				expect("ok" in (await decodeSubscribeResponse(sub.client.reader, version))).toBe(true);
			} else {
				await flush();
			}
			sub.serve(7n);
			sub.serve(9n);
			expect(await sub.servedHeader()).toBe(9n);
			expect(await sub.opensGroup(clock)).toBe(false);
		} finally {
			await sub.close();
		}
	},
);

// Lite-01/02 retain the arrival-only floor even when the answer contains history.
test.each([Version.DRAFT_01, Version.DRAFT_02])(
	"lite %s: a pending join retains its arrival-only floor",
	async (version) => {
		const sub = await servedSubscription({ version, pending: true });
		try {
			await sub.accept([0n, 3n, 7n]);
			expect(await sub.servedHeader()).toBe(0n);
			expect(await sub.servedHeader()).toBe(3n);
			expect(await sub.servedHeader()).toBe(7n);
		} finally {
			await sub.close();
		}
	},
);

// On an empty track the re-pin has no latest group to name, so the first group the
// track produces is served, even below the dropped floor.
test("lite draft-05: an update dropping the floor on an empty track serves the next group", async () => {
	const sub = await servedSubscription({ startGroup: 5n });
	try {
		await replayUpdate({ priority: 0 }).encode(sub.client.writer, Version.DRAFT_05);
		await flush();
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
	} finally {
		await sub.close();
	}
});

// Lite-06 encodes a floor of group 0 as 0, including a subscribe that omitted Group Start.
// That is a floor, so a later group 0 is still served.
test("lite draft-06: an omitted group start still serves a group below the first one", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_06 });
	try {
		sub.serve(2n);
		expect(await sub.servedSequence()).toBe(2n);
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);
	} finally {
		await sub.close();
	}
});

// A group pop is the linearization point. Once the publisher reaches the SUBSCRIBE_START write,
// the group and its range have already been decided, so an update queued during the write applies
// to the next pop rather than reaching backward into this one.
test("lite draft-05: a group popped before a cap update is still served", async () => {
	const sub = await servedSubscription({ startGroup: 1n, gated: true });
	const ranges = spyOn(TrackSubscriber.prototype, "withRange");
	try {
		sub.serve(1n);
		await sub.parked;
		ranges.mockClear();

		await replayUpdate({ priority: 0, endGroup: 0n }).encode(sub.client.writer, Version.DRAFT_05);
		await flush();
		// The full update is visible, but the parked serving loop still owns its local cursor.
		expect(ranges).not.toHaveBeenCalled();

		sub.release();
		expect(await sub.servedSequence()).toBe(1n);
		while (ranges.mock.calls.length === 0) await flush();
		// Dropping the named floor re-pins to the latest group, as an unfloored SUBSCRIBE does.
		expect(lastGroups(ranges)).toEqual({ start: { included: 1n } });
	} finally {
		ranges.mockRestore();
		sub.release();
		await sub.close();
	}
});

// No next read is armed while a subscription response is blocked. A buffered control therefore
// applies before the next buffered group is popped, matching the Rust publisher's control-first
// poll order.
test("lite draft-06: a queued floor update applies before the next group pop", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_06, startGroup: 0n, gated: true });
	const ranges = spyOn(TrackSubscriber.prototype, "withRange");
	try {
		sub.serve(0n);
		await sub.parked;
		sub.serve(1n);

		await replayUpdate({ priority: 0, startGroup: 2n }).encode(sub.client.writer, Version.DRAFT_06);
		sub.serve(2n);
		await flush();
		// Draft-06 group 0 is a floor, so the first group does not raise the cursor.
		// The queued update is still waiting on the parked write.
		expect(ranges).not.toHaveBeenCalled();

		sub.release();

		expect(await sub.servedSequence()).toBe(0n);
		expect(await sub.servedSequence()).toBe(2n);
		expect(lastGroups(ranges)).toEqual({ start: { included: 2n }, end: undefined });
	} finally {
		ranges.mockRestore();
		sub.release();
		await sub.close();
	}
});

// SUBSCRIBE_START pins the floor to the first served group. A later update can still
// widen it, serving the cached groups below the old floor.
test("lite draft-06: a widening update lowers the serving floor", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_06, startGroup: 10n });
	try {
		sub.serve(10n);
		expect(await sub.servedSequence()).toBe(10n);

		await replayUpdate({ priority: 0, startGroup: 5n }).encode(sub.client.writer, Version.DRAFT_06);
		await flush();

		sub.serve(5n);
		sub.serve(6n);
		// Opened together, so the newer group may take the first stream.
		const served = [await sub.servedSequence(), await sub.servedSequence()];
		expect(served.sort()).toEqual([5n, 6n]);
	} finally {
		await sub.close();
	}
});

// The queued update raises the floor within the next buffered group. Control-first polling
// applies the new frame start before popping the group, so excluded frames never reach the wire.
test("lite draft-06: a queued frame floor applies before the next group pop", async () => {
	const sub = await servedSubscription({
		version: Version.DRAFT_06,
		startGroup: 0n,
		frames: ["a", "b", "c"],
		gated: true,
	});
	const ranges = spyOn(TrackSubscriber.prototype, "withRange");
	try {
		sub.serve(0n);
		await sub.parked;
		sub.serve(1n);

		await replayUpdate({ priority: 0, startGroup: 1n, startFrame: 2 }).encode(sub.client.writer, Version.DRAFT_06);
		await flush();
		// Draft-06 group 0 is a floor, so serving the first group does not move the cursor.
		// The queued update is still waiting on the parked write.
		expect(ranges).not.toHaveBeenCalled();

		sub.release();

		expect(await sub.servedGroup()).toEqual({ sequence: 0n, frameStart: 0, payloads: ["a", "b", "c"] });
		expect(await sub.servedGroup()).toEqual({ sequence: 1n, frameStart: 2, payloads: ["c"] });
		expect(lastGroups(ranges)).toEqual({ start: { included: 1n }, end: undefined });
	} finally {
		ranges.mockRestore();
		sub.release();
		await sub.close();
	}
});

// The update and group are both ready when the write unblocks. Control is drained first, then
// the group is popped and positioned synchronously under the new frame cap.
test("lite draft-06: a queued frame update applies before the next group pop", async () => {
	const sub = await servedSubscription({
		version: Version.DRAFT_06,
		startGroup: 0n,
		endGroup: 1n,
		endFrame: 1,
		frames: ["a", "b", "c"],
		gated: true,
	});
	try {
		sub.serve(0n);
		await sub.parked;
		sub.serve(1n);
		await replayUpdate({ priority: 0, endGroup: 1n, endFrame: 0 }).encode(sub.client.writer, Version.DRAFT_06);
		await flush();

		sub.release();

		expect(await sub.servedGroup()).toEqual({ sequence: 0n, frameStart: 0, payloads: ["a", "b", "c"] });
		expect(await sub.servedGroup()).toEqual({ sequence: 1n, frameStart: 0, payloads: ["a"] });
	} finally {
		await sub.close();
	}
});

// The inverse ordering is equally important. Once the group is popped, its range is fixed in
// the same turn, so an update decoded while SUBSCRIBE_START is blocked only affects later groups.
test("lite draft-06: a popped group keeps its frame bounds across a queued update", async () => {
	const sub = await servedSubscription({
		version: Version.DRAFT_06,
		startGroup: 0n,
		endGroup: 0n,
		endFrame: 1,
		frames: ["a", "b", "c"],
		gated: true,
	});
	try {
		sub.serve(0n);
		await sub.parked;
		await replayUpdate({ priority: 0, endGroup: 0n, endFrame: 2 }).encode(sub.client.writer, Version.DRAFT_06);
		await flush();

		sub.release();
		expect(await sub.servedGroup()).toEqual({ sequence: 0n, frameStart: 0, payloads: ["a", "b"] });
	} finally {
		await sub.close();
	}
});

// Updates are full-state, so a burst buffered while the serving loop is parked only needs its
// newest member. Keeping one pending update bounds memory without serving under stale bounds.
test("lite draft-06: a burst of updates coalesces before the next group pop", async () => {
	const sub = await servedSubscription({
		version: Version.DRAFT_06,
		startGroup: 0n,
		endGroup: 1n,
		endFrame: 2,
		frames: ["a", "b", "c"],
		gated: true,
	});
	try {
		// Group 0 parks the loop inside its SUBSCRIBE_START write; group 1 buffers behind it.
		sub.serve(0n);
		await sub.parked;
		sub.serve(1n);
		const ranges = spyOn(TrackSubscriber.prototype, "withRange");

		try {
			// Two updates land back to back, the second superseding the first.
			await replayUpdate({ priority: 0, endGroup: 1n, endFrame: 1 }).encode(sub.client.writer, Version.DRAFT_06);
			await replayUpdate({ priority: 0, endGroup: 1n, endFrame: 0 }).encode(sub.client.writer, Version.DRAFT_06);
			await flush();

			sub.release();

			// Group 0 was popped before either update and is not the end group either way.
			expect(await sub.servedGroup()).toEqual({
				sequence: 0n,
				frameStart: 0,
				payloads: ["a", "b", "c"],
			});
			// Group 1 is popped under the latest state, with no application of the older one.
			expect(await sub.servedGroup()).toEqual({ sequence: 1n, frameStart: 0, payloads: ["a"] });
			expect(ranges).toHaveBeenCalledTimes(1);
			// Draft-06 encodes a floor of group 0 as no floor, which assigns 0.
			expect(lastGroups(ranges)).toEqual({ start: { included: 0n } });
		} finally {
			ranges.mockRestore();
		}
	} finally {
		await sub.close();
	}
});

// Applying one update can itself race the decoder's next message. The serving loop yields a task
// before another pop so the decoder can finish the already-delivered update and tighten the cap.
test("lite draft-06: a newer update wins before a buffered group backlog drains", async () => {
	using clock = manual();
	const sub = await servedSubscription({
		version: Version.DRAFT_06,
		startGroup: 0n,
		gated: true,
	});
	// Encoded up front, so the second update arrives whole the way a peer's would.
	const capped = await Writer.collect(Version.DRAFT_06, (w) =>
		replayUpdate({ priority: 0, endGroup: 1n }).encode(w, Version.DRAFT_06),
	);
	let second!: Promise<void>;
	const withRange = TrackSubscriber.prototype.withRange;
	const ranges = spyOn(TrackSubscriber.prototype, "withRange").mockImplementation(function (
		this: TrackSubscriber,
		groups,
	) {
		second ??= sub.client.writer.write(capped);
		return withRange.call(this, groups);
	});

	try {
		// Group 0 parks the loop. Groups 1 and 2 are both readable when it wakes.
		sub.serve(0n);
		await sub.parked;
		ranges.mockClear();
		sub.serve(1n);
		sub.serve(2n);

		// The first update wakes the serving loop. Applying it starts the second update,
		// which caps the subscription before group 2.
		await replayUpdate({ priority: 0, endGroup: 2n }).encode(sub.client.writer, Version.DRAFT_06);
		await flush();
		sub.release();

		expect(await sub.servedSequence()).toBe(0n);
		expect(await sub.servedSequence()).toBe(1n);
		await second;
		expect(await sub.opensGroup(clock)).toBe(false);
		expect(ranges).toHaveBeenCalledTimes(2);
	} finally {
		ranges.mockRestore();
		await sub.close();
	}
});

// Scheduling affects streams already in flight, so it cannot wait behind a response write.
// The full update remains atomic to observers, while the local range cursor stays serialized.
test("lite draft-06: scheduling updates apply while SUBSCRIBE_START is blocked", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_06, startGroup: 0n, gated: true });
	const ranges = spyOn(TrackSubscriber.prototype, "withRange");
	try {
		sub.serve(0n);
		await sub.parked;
		ranges.mockClear();

		await replayUpdate({ priority: 9, endGroup: 5n }).encode(sub.client.writer, Version.DRAFT_06);
		await flush();

		expect(sub.track.subscription.peek()).toEqual({
			priority: 9,
			// Lite-06 cannot state a minimum.
			delay: { min: null, max: Duration.millis(TEST_MAX_DELAY_MS) },
			order: "descending",
			ranges: [{ end: { absolute: 6n } }],
		});
		expect(ranges).not.toHaveBeenCalled();

		sub.release();
		expect(await sub.servedSequence()).toBe(0n);
		while (ranges.mock.calls.length === 0) await flush();
		expect(lastGroups(ranges)).toEqual({ start: { included: 0n } });
	} finally {
		ranges.mockRestore();
		sub.release();
		await sub.close();
	}
});

// A peer FIN is the subscriber leaving, which the loop takes ahead of any ready group: it stops
// there rather than draining what is buffered, since nobody is reading the rest. Matches the Rust
// publisher's TrackEnd::PeerFin, which drops the in-flight group machines instead of draining.
test("lite draft-05: a peer FIN ends serving while the producer is still live", async () => {
	using clock = manual();
	const sub = await servedSubscription({ startGroup: 0n });
	try {
		sub.serve(0n);
		expect(await sub.servedSequence()).toBe(0n);

		// The subscriber unsubscribes. The producer doesn't know and keeps going.
		sub.client.writer.close();
		await flush();
		sub.serve(1n);

		expect(await sub.opensGroup(clock)).toBe(false);
	} finally {
		await sub.close();
	}
});

// The two halves of a bidirectional stream close independently. A peer FIN cannot unblock a
// response write by itself, so serving must race the write against decoded termination.
test("lite draft-05: a peer FIN interrupts a blocked SUBSCRIBE_START", async () => {
	const sub = await servedSubscription({ startGroup: 0n, gated: true });
	const resets = spyOn(Writer.prototype, "reset");
	try {
		sub.serve(0n);
		await sub.parked;

		sub.client.writer.close();
		await sub.serving;
		expect(resets).toHaveBeenCalledTimes(1);
	} finally {
		resets.mockRestore();
		// Also unwinds the old behavior when this regression fails.
		sub.release();
		await sub.close();
	}
});

// runSubscribe waits on the decoder before it returns, so a subscription that dies mid-write
// with a control still queued has to end the decoder too, or it never returns and the
// subscription leaks.
test("lite draft-05: teardown unwinds with an undelivered update queued", async () => {
	const sub = await servedSubscription({ startGroup: 0n, gated: true });
	try {
		// Group 0 parks the loop inside its SUBSCRIBE_START write.
		sub.serve(0n);
		await sub.parked;

		// The update decodes into the slot behind the parked loop, which never takes it
		// because the write it is parked on fails.
		await replayUpdate({ priority: 0, endGroup: 5n }).encode(sub.client.writer, Version.DRAFT_05);
		await flush();

		sub.release(new Error("write failed"));
		await sub.serving;
	} finally {
		await sub.close();
	}
});

// A Rust subscriber feeds this value straight into `track::Producer::finish_at`, which is
// exclusive, so an inclusive bound here silently truncates the final group across languages.
test("lite draft-05: subscribe end is the exclusive boundary", async () => {
	expect((await subscribeEnd([0, 1, 2])).group).toBe(3n);
});

// recvGroup is arrival-ordered, so the boundary has to clear the max sequence delivered,
// not the last one seen. Otherwise the boundary lands on a group already on the wire.
test("lite draft-05: subscribe end clears the max sequence when groups arrive out of order", async () => {
	expect((await subscribeEnd([0, 2, 1])).group).toBe(3n);
});

// 0 is the only encoding for "no groups at all"; an inclusive bound cannot express it
// without colliding with a track whose sole group was sequence 0.
test("lite draft-05: subscribe end is 0 when no groups were produced", async () => {
	expect((await subscribeEnd([])).group).toBe(0n);
});

// Lite-07 forwards an errored end's error with its boundary, so the subscriber keeps the history.
test("lite draft-07: subscribe end carries the track's error", async () => {
	const end = await subscribeEnd([0, 1], Version.DRAFT_07, new StreamError(StreamCode(71)));
	expect([end.group, end.error]).toEqual([2n, 71]);
	expect((await subscribeEnd([0, 1], Version.DRAFT_07)).error).toBeUndefined();
});

// The boundary clears every produced group even when a sequence was skipped.
test("lite draft-07: subscribe end names the final boundary", async () => {
	const end = await subscribeEnd([0, 2], Version.DRAFT_07);
	expect(end.group).toBe(3n);
});

test("lite draft-07: an empty track ends at zero", async () => {
	const end = await subscribeEnd([], Version.DRAFT_07);
	expect(end.group).toBe(0n);
});

// finishAt declares the boundary while lower groups are still being produced.
test("lite draft-07: subscribe end precedes groups below a declared finish", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_07, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const client = await Stream.open(pair.client, { version: Version.DRAFT_07 });
	const server = await Stream.accept(pair.server, Version.DRAFT_07);
	if (!server) throw new Error("publisher never accepted the subscribe stream");
	void publisher.runSubscribe(
		new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 }),
		server,
	);

	try {
		const first = track.createGroup(0n);
		first.writeFrame(textFrame("hello"));
		first.finish();
		track.finishAt({ group: 2n, frame: 0 });

		const start = await decodeSubscribeResponse(client.reader, Version.DRAFT_07);
		expect("start" in start).toBe(true);
		const pending = decodeSubscribeResponse(client.reader, Version.DRAFT_07);
		const early = await pending;
		expect("finish" in early).toBe(true);

		const second = track.createGroup(1n);
		second.writeFrame(textFrame("hello"));
		second.finish();
		track.finish();
		track.close();

		const resp = await pending;
		if (!("finish" in resp)) throw new Error("expected SUBSCRIBE_FINISH");
		expect(resp.finish.group).toBe(2n);
	} finally {
		publisher.close();
		client.close();
	}
});

// Before lite-07 an errored end goes out only as the reset at that end: a clean SUBSCRIBE_FINISH
// sent early could let the subscriber settle before the reset arrives.
test("lite draft-06: an errored end is not declared early", async () => {
	using clock = manual();
	const pair = createMockTransportPair(ALPN_06);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_06, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const client = await Stream.open(pair.client, { version: Version.DRAFT_06 });
	const server = await Stream.accept(pair.server, Version.DRAFT_06);
	if (!server) throw new Error("publisher never accepted the subscribe stream");
	void publisher.runSubscribe(
		new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 }),
		server,
	);

	try {
		const first = track.createGroup(0n);
		first.writeFrame(textFrame("hello"));
		first.finish();
		track.finishAt({ group: 2n, frame: 0 }, new StreamError(StreamCode(71)));

		const start = await decodeSubscribeResponse(client.reader, Version.DRAFT_06);
		expect("start" in start).toBe(true);
		const pending = decodeSubscribeResponse(client.reader, Version.DRAFT_06);
		expect(await settlesWithin(pending, clock, Duration.millis(20))).toBe(false);

		const second = track.createGroup(1n);
		second.writeFrame(textFrame("hello"));
		second.finish();
		await expect(pending).rejects.toBeDefined();
	} finally {
		publisher.close();
		client.close();
	}
});

// A subscription capped short of an errored end is owed no error, so its boundary still goes out.
test("lite draft-06: a subscription capped short of an errored end gets its boundary", async () => {
	const sub = await servedSubscription({ version: Version.DRAFT_06, startGroup: 0n, endGroup: 1n });
	try {
		for (const sequence of [0n, 1n, 2n, 3n]) sub.serve(sequence);
		sub.track.finishAt({ group: 4n, frame: 0 }, new StreamError(StreamCode(71)));
		for (;;) {
			const resp = await decodeSubscribeResponse(sub.client.reader, Version.DRAFT_06);
			if (!("finish" in resp)) continue;
			expect(resp.finish.group).toBe(4n);
			break;
		}
	} finally {
		await sub.close();
	}
});

// Serves one group with its stream open held until `open(ok)`, and returns the pending
// SUBSCRIBE_FINISH plus the call that lets the open succeed or fail.
async function heldOpenEnd() {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_07, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	let open!: (ok: boolean) => void;
	const opened = new Promise<boolean>((resolve) => {
		open = resolve;
	});
	const createUni = pair.server.createUnidirectionalStream.bind(pair.server);
	spyOn(pair.server, "createUnidirectionalStream").mockImplementation(async (options) => {
		if (!(await opened)) throw new Error("no stream credit");
		return createUni(options);
	});

	const client = await Stream.open(pair.client, { version: Version.DRAFT_07 });
	const server = await Stream.accept(pair.server, Version.DRAFT_07);
	if (!server) throw new Error("publisher never accepted the subscribe stream");
	void publisher.runSubscribe(
		new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 }),
		server,
	);

	const group = track.createGroup(0n);
	group.writeFrame(textFrame("hello"));
	group.finish();
	track.finish();
	track.close();

	const start = await decodeSubscribeResponse(client.reader, Version.DRAFT_07);
	expect("start" in start).toBe(true);
	const end = decodeSubscribeResponse(client.reader, Version.DRAFT_07);

	return {
		end,
		open,
		next: () => decodeSubscribeResponse(client.reader, Version.DRAFT_07),
		close() {
			publisher.close();
			client.close();
		},
	};
}

// The end is known even when a group is waiting for stream credit.
test("lite draft-07: subscribe end precedes a blocked stream open", async () => {
	const held = await heldOpenEnd();
	try {
		const early = await held.end;
		expect("finish" in early).toBe(true);

		held.open(true);
		const resp = await held.end;
		if (!("finish" in resp)) throw new Error("expected SUBSCRIBE_FINISH");
		expect(resp.finish.group).toBe(1n);
	} finally {
		held.close();
	}
});

// A failed open does not prevent the declared boundary from reaching the subscriber.
test("lite draft-07: subscribe end precedes a failed stream open", async () => {
	const held = await heldOpenEnd();
	try {
		held.open(false);
		const resp = await held.end;
		if (!("finish" in resp)) throw new Error("expected SUBSCRIBE_FINISH");
		expect(resp.finish.group).toBe(1n);
		const dropped = await held.next();
		if (!("drop" in dropped)) throw new Error("expected SUBSCRIBE_DROP");
		expect([dropped.drop.start, dropped.drop.end, dropped.drop.error]).toEqual([0n, 1n, 0]);
	} finally {
		held.close();
	}
});

// A stream that fails after its FIN may have lost its header, so the group is still dropped.
test("lite draft-07: a group whose close fails is dropped", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_07, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const createUni = pair.server.createUnidirectionalStream.bind(pair.server);
	spyOn(pair.server, "createUnidirectionalStream").mockImplementation(async (options) => {
		const writer = (await createUni(options)).getWriter();
		return new WritableStream<Uint8Array>({
			write: (chunk) => writer.write(chunk),
			close: () => {
				writer.abort(new Error("connection lost")).catch(() => void 0);
				throw new Error("connection lost");
			},
		});
	});

	const client = await Stream.open(pair.client, { version: Version.DRAFT_07 });
	const server = await Stream.accept(pair.server, Version.DRAFT_07);
	if (!server) throw new Error("publisher never accepted the subscribe stream");
	void publisher.runSubscribe(
		new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 }),
		server,
	);

	try {
		const group = track.createGroup(0n);
		group.writeFrame(textFrame("hello"));
		group.finish();
		track.finish();
		track.close();

		const start = await decodeSubscribeResponse(client.reader, Version.DRAFT_07);
		expect("start" in start).toBe(true);
		const end = await decodeSubscribeResponse(client.reader, Version.DRAFT_07);
		if (!("finish" in end)) throw new Error("expected SUBSCRIBE_FINISH");
		const dropped = await decodeSubscribeResponse(client.reader, Version.DRAFT_07);
		if (!("drop" in dropped)) throw new Error("expected SUBSCRIBE_DROP");
		expect([dropped.drop.start, dropped.drop.end, dropped.drop.error]).toEqual([0n, 1n, 0]);
	} finally {
		publisher.close();
		client.close();
	}
});

test("blocked DROP responses stop opening later groups", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_07, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null });
	const client = await Stream.open(pair.client, { version: Version.DRAFT_07 });
	const server = await Stream.accept(pair.server, Version.DRAFT_07);
	if (!server) throw new Error("missing subscribe stream");
	const opens = spyOn(pair.server, "createUnidirectionalStream").mockImplementation(async () => {
		throw new Error("no stream credit");
	});
	let release!: () => void;
	const gate = new Promise<void>((resolve) => {
		release = resolve;
	});
	let blocked!: () => void;
	const waiting = new Promise<void>((resolve) => {
		blocked = resolve;
	});
	const write = server.writer.write.bind(server.writer);
	let held = false;
	const writes = spyOn(server.writer, "write").mockImplementation(async (chunk) => {
		if (!held && chunk.length === 1 && chunk[0] === 2) {
			held = true;
			blocked();
			await gate;
		}
		await write(chunk);
	});
	const append = (sequence: number) => {
		const group = track.createGroup(BigInt(sequence));
		group.writeFrame(textFrame("frame"));
		group.finish();
	};
	for (let sequence = 0; sequence < 8; sequence++) append(sequence);
	const serving = publisher.runSubscribe(
		replaySubscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 }),
		server,
	);
	try {
		expect("start" in (await decodeSubscribeResponse(client.reader, Version.DRAFT_07))).toBe(true);
		await waiting;
		expect(opens).toHaveBeenCalledTimes(8);
		for (let sequence = 8; sequence < 72; sequence++) append(sequence);
		// One executor turn drains pending signal notifications without advancing time.
		await new Promise<void>((resolve) => setImmediate(resolve));
		expect(opens).toHaveBeenCalledTimes(8);
		release();
		track.finish();
		track.close();
		let dropped = 0;
		// Lite-07 keeps the stream open past SUBSCRIBE_FINISH, so read until every group is named.
		while (dropped < 72) {
			const response = await decodeSubscribeResponseMaybe(client.reader, Version.DRAFT_07);
			if (!response) break;
			if ("drop" in response) dropped += Number(response.drop.end - response.drop.start);
		}
		expect(dropped).toBe(72);
		expect(opens).toHaveBeenCalledTimes(72);
		client.writer.close();
		await serving;
	} finally {
		release();
		writes.mockRestore();
		opens.mockRestore();
		publisher.close();
		client.close();
		broadcast.close();
	}
});

/** One group stream the publisher put on the wire. */
type Served = { sequence: bigint; frameStart: number; payloads: string[] };

/**
 * Serves `groups` (frame payloads per group, keyed by sequence) under `bounds`, and
 * reports every group stream that reached the wire plus the resolved
 * SUBSCRIBE_START / SUBSCRIBE_FINISH range. A group in `received` arrived as a tail
 * starting at that frame.
 */
async function serve(
	groups: Record<number, string[]>,
	bounds: { startGroup?: bigint; startFrame?: number; endGroup?: bigint; endFrame?: number },
	received: Record<number, number> = {},
): Promise<{ start?: bigint; end?: bigint; served: Served[] }> {
	using clock = manual();
	const pair = createMockTransportPair(ALPN_06);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_06, randomHop(), origin.consume());

	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const client = await Stream.open(pair.client, { version: Version.DRAFT_06 });
	const server = await Stream.accept(pair.server, Version.DRAFT_06);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({
		id: 0n,
		broadcast: Path.from("test"),
		track: "video",
		priority: 0,
		startGroup: bounds.startGroup,
		startFrame: bounds.startFrame ?? 0,
		endGroup: bounds.endGroup,
		endFrame: bounds.endFrame,
	});
	void publisher.runSubscribe(msg, server);

	for (const [sequence, frames] of Object.entries(groups)) {
		const start = received[Number(sequence)];
		const group =
			start === undefined
				? track.createGroup(BigInt(sequence))
				: hooks.receiveGroup(track, { sequence: BigInt(sequence), start });
		for (const frame of frames) group.writeFrame(textFrame(frame));
		group.finish();

		// Let the publisher drain each group before the next, so the streams arrive in
		// the order written rather than whatever the cache hands over in one batch.
		await clock.advance(Duration.ZERO);
	}
	track.finish();
	track.close();

	const reader = pair.client.incomingUnidirectionalStreams.getReader();
	try {
		const served: Served[] = [];
		for (;;) {
			// The publisher may simply have nothing more to send, so give the read an idle
			// window. Cancel rather than release below: releasing with a read still pending
			// would throw.
			const read = reader.read();
			if (!(await settlesWithin(read, clock, Duration.millis(50)))) break;
			const next = await read;
			if (next.done) break;

			const stream = new Reader(next.value, undefined, Version.DRAFT_06);
			await stream.u53(); // stream type
			const header = await GroupMessage.decode(stream, Version.DRAFT_06);

			const payloads: string[] = [];
			while (!(await stream.done())) {
				// Each frame is a zigzag timestamp delta, then a length-prefixed payload.
				await stream.u62();
				payloads.push(new TextDecoder().decode(await stream.read(await stream.u53())));
			}
			served.push({ sequence: header.sequence, frameStart: header.frameStart, payloads });
		}

		let start: bigint | undefined;
		let end: bigint | undefined;
		for (;;) {
			const resp = await decodeSubscribeResponse(client.reader, Version.DRAFT_06);
			if ("start" in resp) start = resp.start.starts[0];
			if ("finish" in resp) {
				end = resp.finish.group;
				break;
			}
		}
		return { start, end, served };
	} finally {
		// Settles the pending read as well as dropping the stream.
		await reader.cancel();
		broadcast.close();
		client.close();
	}
}

// The response has no per-frame index, so the receiver numbers what it gets from the
// GROUP header. Ignoring the requested start would relabel frame 0 as frame N.
test("lite draft-06: a subscription starting mid-group skips the head", async () => {
	const { served } = await serve({ 0: ["a", "b", "c", "d"] }, { startGroup: 0n, startFrame: 2 });
	expect(served).toEqual([{ sequence: 0n, frameStart: 2, payloads: ["c", "d"] }]);
});

// A start at the group's final frame count is a valid, empty range: FIN, don't reset.
// A relay resuming a parked track asks for exactly this.
test("lite draft-06: a subscription starting at the end of a group serves it empty", async () => {
	const { start, served } = await serve({ 0: ["a", "b"] }, { startGroup: 0n, startFrame: 2 });
	expect(start).toBe(0n);
	expect(served).toEqual([{ sequence: 0n, frameStart: 2, payloads: [] }]);
});

// A received tail that ends where it starts is the same valid, empty range.
test("lite draft-06: an empty received tail is served empty at its start", async () => {
	const { served } = await serve({ 0: [] }, { startGroup: 0n, startFrame: 5 }, { 0: 5 });
	expect(served).toEqual([{ sequence: 0n, frameStart: 5, payloads: [] }]);
});

// The end bound is inclusive.
test("lite draft-06: a subscription capped mid-group stops at the end frame", async () => {
	const { served } = await serve(
		{ 0: ["a", "b", "c", "d"] },
		{ startGroup: 0n, startFrame: 1, endGroup: 0n, endFrame: 2 },
	);
	expect(served).toEqual([{ sequence: 0n, frameStart: 1, payloads: ["b", "c"] }]);
});

// The default is the whole group, byte-identical to a draft with no such field.
test("lite draft-06: an unbounded subscription serves the whole group", async () => {
	const { served } = await serve({ 0: ["a", "b"] }, {});
	expect(served).toEqual([{ sequence: 0n, frameStart: 0, payloads: ["a", "b"] }]);
});

// The frame bounds qualify their own group and nothing else. Without a span like this,
// an implementation that caps every group passes just as well.
test("lite draft-06: frame bounds apply only to the groups they name", async () => {
	const { served } = await serve(
		{ 0: ["a0", "a1", "a2"], 1: ["b0", "b1", "b2"], 2: ["c0", "c1", "c2"] },
		{ startGroup: 0n, startFrame: 2, endGroup: 2n, endFrame: 0 },
	);
	expect(served).toEqual([
		{ sequence: 0n, frameStart: 2, payloads: ["a2"] },
		// Between the bounds: served whole, from frame 0, with no cap.
		{ sequence: 1n, frameStart: 0, payloads: ["b0", "b1", "b2"] },
		{ sequence: 2n, frameStart: 0, payloads: ["c0"] },
	]);
});

// A group below the requested start was never asked for. Serving it also lets
// SUBSCRIBE_START name a group below the start, which the draft forbids.
test("lite draft-06: groups below the start group are not served", async () => {
	const { start, served } = await serve({ 0: ["x"], 1: ["y"], 2: ["z"] }, { startGroup: 1n });
	expect(served.map((s) => s.sequence)).toEqual([1n, 2n]);
	expect(start).toBe(1n);
});

// The end group is inclusive; anything past it is outside the subscription.
test("lite draft-06: groups past the end group are not served", async () => {
	const { served } = await serve({ 0: ["w"], 1: ["x"], 2: ["y"], 3: ["z"] }, { startGroup: 1n, endGroup: 2n });
	expect(served.map((s) => s.sequence)).toEqual([1n, 2n]);
});

// SUBSCRIBE_FINISH names the track's exclusive final boundary, not the capped delivered
// range: a Rust peer feeds it into finish_at, so a truncated value would silently drop
// the held-back groups across languages.
test("lite draft-06: a capped subscription still reports the track's final boundary", async () => {
	const { end } = await serve({ 0: ["w"], 1: ["x"], 2: ["y"], 3: ["z"] }, { startGroup: 1n, endGroup: 2n });
	expect(end).toBe(4n);
});

// Group streams open with waitUntilAvailable, so a browser at its concurrent stream cap
// parks the open until the peer frees a slot. That can outlast the subscription, and the
// queued group holds its frames the whole time.
// Serves one finished group to a subscriber over a transport that has no stream slot free,
// so the group sits queued inside the open until `freeSlot` is called. `outcome` settles
// with what became of the group once the slot frees, so no test has to guess at a delay.
async function saturatedGroup() {
	const pair = createMockTransportPair(ALPN_05);

	let freeSlot!: () => void;
	const slot = new Promise<void>((resolve) => {
		freeSlot = resolve;
	});

	let opening!: () => void;
	const opened = new Promise<void>((resolve) => {
		opening = resolve;
	});

	let reset!: () => void;
	let wrote!: () => void;
	const outcome = Promise.race([
		new Promise<"reset">((resolve) => {
			reset = () => resolve("reset");
		}),
		new Promise<"sent">((resolve) => {
			wrote = () => resolve("sent");
		}),
	]);

	const groupStream = new WritableStream<Uint8Array>({ write: () => wrote(), abort: () => reset() });

	// Stand in for a transport at its stream cap, which parks the open the way a browser does
	// rather than rejecting it, whatever we asked for.
	const requested: unknown[] = [];
	pair.server.createUnidirectionalStream = async (options?: unknown) => {
		requested.push(options);
		opening();
		await slot;
		return groupStream;
	};

	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	const client = await Stream.open(pair.client, { version: Version.DRAFT_05 });
	const server = await Stream.accept(pair.server, Version.DRAFT_05);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const msg = new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 });
	void publisher.runSubscribe(msg, server);

	// A finished group still has frames to send, so its own close must not drop it.
	const group = track.createGroup(0n);
	group.writeFrame(textFrame("hello"));
	group.finish();

	// The group is queued inside the open from here on.
	await opened;

	return {
		client,
		track,
		freeSlot,
		outcome,
		requested,
		close: () => {
			publisher.close();
			broadcast.close();
		},
	};
}

// Group streams wait for the peer's stream limit like any open: the line, not the transport,
// decides which waiting open a returning slot goes to.
test("lite draft-05: group streams wait for a slot", async () => {
	const { requested, freeSlot, close } = await saturatedGroup();

	expect(requested).toEqual([{ sendOrder: expect.any(Number), waitUntilAvailable: true }]);

	freeSlot();
	close();
});

// The header is part of the group's lifetime too. If it blocks on flow control, advancing
// the live edge must reset the stream without waiting for that write to finish.
test("lite draft-05: a blocked group header is reset when the group expires", async () => {
	const pair = createMockTransportPair(ALPN_05);

	let started!: () => void;
	const headerStarted = new Promise<void>((resolve) => {
		started = resolve;
	});
	let release!: () => void;
	const blocked = new Promise<void>((resolve) => {
		release = resolve;
	});
	let reset!: () => void;
	const streamReset = new Promise<void>((resolve) => {
		reset = resolve;
	});
	const closed = new Promise<void>(() => {});
	const writable = {
		getWriter: () => ({
			closed,
			write: async () => {
				started();
				await blocked;
			},
			close: async () => {},
			abort: async () => {
				reset();
			},
		}),
		abort: async () => {},
	} as unknown as WritableStream<Uint8Array>;
	pair.server.createUnidirectionalStream = async () => writable;

	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const client = await Stream.open(pair.client, { version: Version.DRAFT_05 });
	const server = await Stream.accept(pair.server, Version.DRAFT_05);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	try {
		void publisher.runSubscribe(
			new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 }),
			server,
		);

		const old = track.createGroup(0n);
		old.writeFrame({ payload: new TextEncoder().encode("old"), timestamp: Timestamp.fromMillis(0) });
		old.finish();
		await headerStarted;

		const edge = track.createGroup(1n);
		edge.writeFrame({ payload: new TextEncoder().encode("edge"), timestamp: Timestamp.fromMillis(1000) });
		edge.finish();

		await streamReset;
	} finally {
		release();
		publisher.close();
		client.close();
		broadcast.close();
	}
});

test("lite draft-05: a group waiting for a stream slot is dropped when the subscriber leaves", async () => {
	using clock = manual();
	const { client, track, freeSlot, outcome, close } = await saturatedGroup();

	client.close();

	// Seeing the close is what cancels the queued open, so wait for the publisher to drop
	// the subscription rather than racing it against the slot below.
	while (track.subscription.peek() !== undefined) await track.subscription.changed();

	// The slot is kept for the next open, never used to send the dropped group.
	freeSlot();
	expect(await settlesWithin(outcome, clock, Duration.millis(20))).toBe(false);

	close();
});

// The publisher FINs the subscribe stream itself once a track ends, which must not be
// mistaken for the subscriber leaving: SUBSCRIBE_FINISH counts those queued groups as
// delivered, so dropping them here would strand the tail of every finite track. The FIN
// tells the subscriber every group is accounted for, so it waits for the queued group.
test("lite draft-05: a group waiting for a stream slot survives the track finishing", async () => {
	using clock = manual();
	const { client, track, freeSlot, outcome, close } = await saturatedGroup();

	track.finish();
	track.close();

	// SUBSCRIBE_FINISH goes out while the group is still waiting for its slot.
	for (;;) {
		const resp = await decodeSubscribeResponse(client.reader, Version.DRAFT_05);
		if ("finish" in resp) break;
	}

	// The FIN holds until the queued group is on the wire.
	const fin = client.reader.closed.then(() => "fin" as const);
	expect(await settlesWithin(fin, clock, Duration.millis(20))).toBe(false);

	freeSlot();
	expect(await outcome).toBe("sent");
	expect(await fin).toBe("fin");

	close();
});

// `smoothedRtt` is a DOMHighResTimeStamp, so a real browser hands back a fractional
// millisecond. The varint encoder converts with `BigInt`, which throws on a fraction,
// and `runProbe`'s catch would then close the probe stream for the rest of the
// session. Drive the real loop so removing the rounding fails this test.
test("runProbe rounds a fractional smoothedRtt instead of killing the stream", async () => {
	using clock = manual();
	const pair = createMockTransportPair(ALPN_05, {
		stats: { estimatedSendRate: 1_000_000, smoothedRtt: 12.34 },
	});
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomHop());

	// The subscriber opens the probe stream; the publisher only replies on it.
	const client = await Stream.open(pair.client, { version: Version.DRAFT_05 });
	const server = await Stream.accept(pair.server, Version.DRAFT_05);
	if (!server) throw new Error("publisher never accepted the probe stream");

	// `runProbe` loops until the stream closes, so close it rather than leaving the
	// task running past the end of the test.
	const probing = publisher.runProbe(server);
	try {
		// The first report waits out the probe interval.
		const probe = await clock.drive(ProbeMessage.decodeMaybe(client.reader, Version.DRAFT_05));
		expect(probe).toBeDefined();
		expect(probe?.rtt).toBe(12);
		expect(probe?.bitrate).toBe(1_000_000);
	} finally {
		client.close();
		await probing;
	}
});

test("a version without the latency field serves a non-dropping budget", async () => {
	for (const version of [Version.DRAFT_01, Version.DRAFT_02]) {
		const sub = await servedSubscription({ version, maxAge: Duration.millis(Number.MAX_SAFE_INTEGER) });
		try {
			// These drafts decode the absent field as zero. The publisher must not turn
			// that into a live-edge request the peer never made.
			expect(sub.track.subscription.peek()?.delay?.max).toEqual(Duration.millis(Number.MAX_SAFE_INTEGER));
		} finally {
			await sub.close();
		}
	}
});

// A group parked at its tail when the budget gives up resets as too old. A FIN would tell
// the next hop the frames it already has are the whole group, and it would cache them so.
test("lite draft-05: a group parked at its tail resets as too old when it expires", async () => {
	const pair = createMockTransportPair(ALPN_05);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const frame = (ms: number) => ({ payload: new TextEncoder().encode("x"), timestamp: Timestamp.fromMillis(ms) });

	const client = await Stream.open(pair.client, { version: Version.DRAFT_05 });
	const server = await Stream.accept(pair.server, Version.DRAFT_05);
	if (!server) throw new Error("publisher never accepted the subscribe stream");
	const opened = pair.client.incomingUnidirectionalStreams.getReader();
	// Left open: its producer never finishes it.
	const open = track.createGroup(0n);

	try {
		void publisher.runSubscribe(
			new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0, maxDelay: 500 }),
			server,
		);
		open.writeFrame(frame(0));

		const next = await opened.read();
		if (next.done) throw new Error("publisher never opened the group stream");
		const stream = new Reader(next.value, undefined, Version.DRAFT_05);
		await stream.u53(); // stream type
		await GroupMessage.decode(stream, Version.DRAFT_05);
		await stream.u62(); // timestamp delta
		await stream.read(await stream.u53());

		// The track runs a second past group 0's successor while group 0 waits for more.
		for (const ms of [1000, 2000]) {
			const group = track.createGroup(BigInt(ms / 1000));
			group.writeFrame(frame(ms));
			group.finish();
		}

		const end = await stream.done().then(
			() => "fin",
			(err: unknown) => err,
		);
		expect(end).toMatchObject({ code: StreamCode.Old });
	} finally {
		opened.releaseLock();
		open.finish();
		publisher.close();
		client.close();
		broadcast.close();
	}
});

// A group can go stale while its stream is still opening. Serving it must abandon the group
// without starting a write: an abandoned write rejects once the stream resets, and nothing
// would handle it (Node exits on the first unhandled rejection).
test("lite draft-05: a group that goes stale while its stream opens writes nothing", async () => {
	const unhandled: unknown[] = [];
	const onUnhandled = (reason: unknown) => unhandled.push(reason);

	const pair = createMockTransportPair(ALPN_05);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_05, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });

	let requested!: () => void;
	const opening = new Promise<void>((resolve) => {
		requested = resolve;
	});
	let open!: () => void;
	const opened = new Promise<void>((resolve) => {
		open = resolve;
	});
	let writes = 0;
	const stale = new WritableStream<Uint8Array>({
		write() {
			writes++;
			throw new Error("write into an abandoned stream");
		},
	});
	spyOn(pair.server, "createUnidirectionalStream").mockImplementationOnce(async () => {
		requested();
		await opened;
		return stale;
	});

	const client = await Stream.open(pair.client, { version: Version.DRAFT_05 });
	const server = await Stream.accept(pair.server, Version.DRAFT_05);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	try {
		process.on("unhandledRejection", onUnhandled);
		void publisher.runSubscribe(
			new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0, maxDelay: 100 }),
			server,
		);

		const write = (sequence: bigint, ms: number) => {
			const group = track.createGroup(sequence);
			group.writeFrame({ payload: new TextEncoder().encode("frame"), timestamp: Timestamp.fromMillis(ms) });
			group.finish();
		};
		write(0n, 0);
		await opening;

		// A group beyond the edge, so group 0's reach (where group 1 begins) is provably past
		// the budget: a successor alone never convicts it.
		write(1n, 10_000);
		write(2n, 20_000);
		open();

		// The slot goes to the next open rather than carrying the stale group.
		await flush();
		expect(writes).toBe(0);
		expect(unhandled).toEqual([]);
	} finally {
		process.off("unhandledRejection", onUnhandled);
		publisher.close();
		client.close();
		broadcast.close();
		origin.close();
	}
});

function grantOf(publish: string): Grant {
	return { publish: new Path.Patterns([Path.Pattern.subtree(publish)]), subscribe: new Path.Patterns([]) };
}

// The resets carrying our UNAUTHORIZED code, by message.
function unauthorizedResets(resets: { mock: { calls: unknown[][] } }): string[] {
	return resets.mock.calls
		.map(([reason]) => reason as Error)
		.filter((reason) => toStreamCode(reason) === StreamCode.Unauthorized)
		.map((reason) => reason.message);
}

// Nothing is announced before the grant it would be checked against: an ANNOUNCE_REQUEST that
// lands before the setup token is answered waits, then leaves out what the grant excludes.
test("lite draft-06: announces wait for the setup grant", async () => {
	using clock = manual();
	const pair = createMockTransportPair(ALPN_06);
	const origin = new OriginProducer();
	const grant = new Signal<Grant | undefined>(undefined);
	let answered = () => {};
	const ready = new Promise<void>((resolve) => {
		answered = resolve;
	});
	const publisher = new Publisher(pair.server, Version.DRAFT_06, randomHop(), origin.consume(), { grant, ready });
	const broadcastRoute = origin.route(Path.from("foo/bar"));
	const broadcast = broadcastRoute.broadcast;

	const written: Uint8Array[] = [];
	const stream = new Stream({
		version: Version.DRAFT_06,
		readable: new ReadableStream<Uint8Array>(),
		writable: new WritableStream<Uint8Array>({
			write(chunk) {
				written.push(chunk);
			},
		}),
	});
	const settle = () => clock.advance(Duration.ZERO);
	const running = publisher.runAnnounce(new AnnounceRequest(Path.empty()), stream);
	await settle();
	expect(written.length).toBe(0);

	grant.set(grantOf("baz"));
	answered();
	await settle();
	// ANNOUNCE_OK alone: the grant excludes foo/bar, so it never reaches the wire.
	const bytes = written.flatMap((chunk) => [...chunk]);
	expect(new TextDecoder().decode(new Uint8Array(bytes))).not.toContain("foo/bar");

	stream.close();
	await running;
	publisher.close();
	broadcast.close();
	origin.close();
	pair.client.close();
	pair.server.close();
});

// A local close while an announce waits on the setup grant finishes it at once, rather than
// holding the close until its deadline.
test("lite draft-06: closing while announces wait for the setup grant does not hang", async () => {
	using clock = manual();
	const pair = createMockTransportPair(ALPN_06);
	const origin = new OriginProducer();
	const grant = new Signal<Grant | undefined>(undefined);
	const publisher = new Publisher(pair.server, Version.DRAFT_06, randomHop(), origin.consume(), {
		grant,
		ready: new Promise<void>(() => {}),
	});

	const written: Uint8Array[] = [];
	const stream = new Stream({
		version: Version.DRAFT_06,
		readable: new ReadableStream<Uint8Array>(),
		writable: new WritableStream<Uint8Array>({
			write(chunk) {
				written.push(chunk);
			},
		}),
	});
	const running = publisher.runAnnounce(new AnnounceRequest(Path.empty()), stream);
	await clock.advance(Duration.ZERO);

	// Without any time passing.
	expect(await settlesWithin(publisher.drain(), clock)).toBe(true);
	await running;
	expect(written.length).toBe(0);

	publisher.close();
	origin.close();
	pair.client.close();
	pair.server.close();
});

// A TRACK is checked again once its broadcast and track resolve, so a shrink in between
// refuses it rather than sending TRACK_INFO the grant no longer covers.
test("lite draft-06: a grant that shrinks while a TRACK resolves refuses it", async () => {
	const pair = createMockTransportPair(ALPN_06);
	const origin = new OriginProducer();
	const handle = origin.claim(Path.from("live"));
	const requests = handle.requested();
	const grant = new Signal<Grant | undefined>(grantOf("live"));
	const publisher = new Publisher(pair.server, Version.DRAFT_06, randomHop(), origin.consume(), {
		grant,
		ready: Promise.resolve(),
	});

	const client = await Stream.open(pair.client, { version: Version.DRAFT_06 });
	const server = await Stream.accept(pair.server, Version.DRAFT_06);
	if (!server) throw new Error("publisher never accepted the track stream");

	const resets = spyOn(Writer.prototype, "reset");
	const produced = new BroadcastProducer();
	produced.createTrack("video", { timescale: Timescale.MILLI });
	try {
		const serving = publisher.runTrackInfo(new TrackMessage(Path.from("live/cam"), "video"), server);

		// The TRACK is parked resolving the broadcast when the grant shrinks.
		const request = await requests.next();
		if (request.done) throw new Error("the handler never saw the request");
		grant.set(grantOf("other"));
		request.value.serve(produced);
		await serving;

		expect(unauthorizedResets(resets)).toContain("unauthorized: live/cam");
	} finally {
		resets.mockRestore();
		produced.close();
		handle.close();
		publisher.close();
		client.close();
		origin.close();
	}
});

// A TRACK_INFO blocked on flow control when the grant shrinks is reset: the rest of the
// answer never reaches the wire once the stream drains.
test("lite draft-06: a grant that shrinks while TRACK_INFO is blocked resets it", async () => {
	const pair = createMockTransportPair(ALPN_06);
	const origin = new OriginProducer();
	const handle = origin.claim(Path.from("live"));
	const requests = handle.requested();
	const grant = new Signal<Grant | undefined>(grantOf("live"));
	const publisher = new Publisher(pair.server, Version.DRAFT_06, randomHop(), origin.consume(), {
		grant,
		ready: Promise.resolve(),
	});

	// The first write (the length prefix) parks until released.
	let release = () => {};
	const blocked = new Promise<void>((resolve) => {
		release = resolve;
	});
	let parked = () => {};
	const writing = new Promise<void>((resolve) => {
		parked = resolve;
	});
	const written: Uint8Array[] = [];
	const stream = new Stream({
		version: Version.DRAFT_06,
		readable: new ReadableStream<Uint8Array>(),
		writable: new WritableStream<Uint8Array>({
			async write(chunk) {
				parked();
				await blocked;
				written.push(chunk);
			},
		}),
	});

	const resets = spyOn(Writer.prototype, "reset");
	const produced = new BroadcastProducer();
	produced.createTrack("video", { timescale: Timescale.MILLI });
	try {
		const serving = publisher.runTrackInfo(new TrackMessage(Path.from("live/cam"), "video"), stream);
		const request = await requests.next();
		if (request.done) throw new Error("the handler never saw the request");
		request.value.serve(produced);
		await writing;

		grant.set(grantOf("other"));
		release();
		await serving;

		expect(unauthorizedResets(resets)).toContain("unauthorized: live/cam");
		// Only the in-flight length prefix; the body never followed it.
		expect(written.length).toBe(1);
	} finally {
		resets.mockRestore();
		produced.close();
		handle.close();
		publisher.close();
		origin.close();
		pair.client.close();
		pair.server.close();
	}
});

// The grant watch is armed before the first check, so a shrink that lands while the broadcast
// is still resolving resets the subscription instead of being missed for good.
test("lite draft-06: a grant that shrinks while the broadcast resolves resets the subscription", async () => {
	const pair = createMockTransportPair(ALPN_06);
	const origin = new OriginProducer();
	const handle = origin.claim(Path.from("live"));
	const requests = handle.requested();
	const grant = new Signal<Grant | undefined>(grantOf("live"));
	const publisher = new Publisher(pair.server, Version.DRAFT_06, randomHop(), origin.consume(), {
		grant,
		ready: Promise.resolve(),
	});

	const client = await Stream.open(pair.client, { version: Version.DRAFT_06 });
	const server = await Stream.accept(pair.server, Version.DRAFT_06);
	if (!server) throw new Error("publisher never accepted the subscribe stream");

	const resets = spyOn(Writer.prototype, "reset");
	const produced = new BroadcastProducer();
	try {
		const msg = new Subscribe({ id: 0n, broadcast: Path.from("live/cam"), track: "video", priority: 0 });
		const serving = publisher.runSubscribe(msg, server);

		// The subscription is parked resolving the broadcast when the grant shrinks.
		const request = await requests.next();
		if (request.done) throw new Error("the handler never saw the request");
		grant.set(grantOf("other"));
		await Promise.resolve();
		request.value.serve(produced);
		await serving;

		expect(unauthorizedResets(resets)).toContain("unauthorized: live/cam");
	} finally {
		resets.mockRestore();
		produced.close();
		handle.close();
		publisher.close();
		client.close();
		origin.close();
	}
});

// A fetch holds its grant watch until the last frame: a group can stay open as long as its
// track, so a check at accept alone would keep serving after a shrink.
test("lite draft-06: a grant that shrinks mid-fetch resets the fetch", async () => {
	const pair = createMockTransportPair(ALPN_06);
	const origin = new OriginProducer();
	const grant = new Signal<Grant | undefined>(grantOf("live"));
	const publisher = new Publisher(pair.server, Version.DRAFT_06, randomHop(), origin.consume(), {
		grant,
		ready: Promise.resolve(),
	});

	const broadcast = publish(origin, Path.from("live"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	// Left open, so the fetch is still serving when the grant shrinks.
	const group = track.createGroup(0n);
	group.writeFrame(textFrame("first"));

	const client = await Stream.open(pair.client, { version: Version.DRAFT_06 });
	const server = await Stream.accept(pair.server, Version.DRAFT_06);
	if (!server) throw new Error("publisher never accepted the fetch stream");

	const resets = spyOn(Writer.prototype, "reset");
	try {
		const msg = new Fetch({ broadcast: Path.from("live"), track: "video", priority: 0, group: 0n });
		const serving = publisher.runFetch(msg, server);
		// The first frame is on the wire before the grant shrinks.
		await client.reader.u8();

		grant.set(grantOf("other"));
		await serving;

		expect(unauthorizedResets(resets)).toContain("unauthorized: live");
	} finally {
		resets.mockRestore();
		group.finish();
		publisher.close();
		client.close();
		broadcast.close();
		origin.close();
	}
});

// A FIN would tell the requester the prefix it got is the whole group.
test("lite draft-07: a fetch whose budget gives up partway resets as too old", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, Version.DRAFT_07, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("live"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const frame = (ms: number) => ({ payload: new TextEncoder().encode("x"), timestamp: Timestamp.fromMillis(ms) });
	// Left open, so the fetch is still serving when the track runs on.
	const open = track.createGroup(0n);
	open.writeFrame(frame(0));

	const client = await Stream.open(pair.client, { version: Version.DRAFT_07 });
	const server = await Stream.accept(pair.server, Version.DRAFT_07);
	if (!server) throw new Error("publisher never accepted the fetch stream");

	const resets = spyOn(Writer.prototype, "reset");
	try {
		const msg = new Fetch({
			broadcast: Path.from("live"),
			track: "video",
			priority: 0,
			maxDelay: 500,
			group: 0n,
		});
		const serving = publisher.runFetch(msg, server);
		// The first frame is on the wire before the track runs a second past its successor.
		await client.reader.u8();
		for (const ms of [1000, 2000]) {
			const group = track.createGroup(BigInt(ms / 1000));
			group.writeFrame(frame(ms));
			group.finish();
		}
		await serving;

		expect(resets.mock.calls.map(([reason]) => toStreamCode(reason as Error))).toContain(StreamCode.Old);
	} finally {
		resets.mockRestore();
		open.finish();
		publisher.close();
		client.close();
		broadcast.close();
		origin.close();
	}
});

test.each([0, 1])("lite draft-07 reports the cached largest position when starting at group %s", async (start) => {
	const startGroup = BigInt(start);
	const version = Version.DRAFT_07;
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("quiet"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const group = track.createGroup(0n);
	group.writeFrame(textFrame("cached"));
	group.finish();
	const client = await Stream.open(pair.client, { version });
	const server = await Stream.accept(pair.server, version);
	if (!server) throw new Error("missing subscribe stream");
	const running = publisher.runSubscribe(
		replaySubscribe({
			id: 0n,
			broadcast: Path.from("quiet"),
			track: "video",
			priority: 0,
			startGroup,
		}),
		server,
	);
	try {
		const response = await decodeSubscribeResponse(client.reader, version);
		if (!("start" in response)) throw new Error("expected SUBSCRIBE_OK");
		expect(response.start.starts[0]).toBe(startGroup);
		expect(response.start.largest).toEqual({ group: 0n, frame: 0 });
	} finally {
		client.close();
		publisher.close();
		broadcast.close();
		origin.close();
		pair.client.close();
		pair.server.close();
		await running;
	}
});

// A relay that holds only the first group of a backlog answers with the largest its source
// reported, so a live joiner downstream starts at the newest group rather than the oldest one
// the budget serves.
test("lite draft-07 reports the source's largest position past what a relay holds", async () => {
	const version = Version.DRAFT_07;
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("relayed"));
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	// The source answered with group 4 as its largest; only group 0 has landed.
	hooks.reportedLargest(track, { group: 4n, frame: 0 });
	const group = track.createGroup(0n);
	group.writeFrame(textFrame("oldest"));
	group.finish();
	const client = await Stream.open(pair.client, { version });
	const server = await Stream.accept(pair.server, version);
	if (!server) throw new Error("missing subscribe stream");
	const running = publisher.runSubscribe(
		replaySubscribe({ id: 0n, broadcast: Path.from("relayed"), track: "video", priority: 0 }),
		server,
	);
	try {
		const response = await decodeSubscribeResponse(client.reader, version);
		if (!("start" in response)) throw new Error("expected SUBSCRIBE_START");
		expect([response.start.starts[0], response.start.largest]).toEqual([0n, { group: 4n, frame: 0 }]);
	} finally {
		client.close();
		publisher.close();
		broadcast.close();
		origin.close();
		pair.client.close();
		pair.server.close();
		await running;
	}
});

for (const version of [Version.DRAFT_03, Version.DRAFT_04, Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07]) {
	test(`Report consumes PROBE targets and observes their FIN (${version})`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		const pair = createMockTransportPair(ALPN_05, { stats: { estimatedSendRate: 1_000_000, smoothedRtt: 12 } });
		const publisher = new Publisher(pair.server, version, randomHop());
		const client = await Stream.open(pair.client, { version });
		const server = await Stream.accept(pair.server, version);
		if (!server) throw new Error("publisher did not accept PROBE");
		const reset = spyOn(server.writer, "reset");
		const close = spyOn(server.writer, "close");
		const serving = publisher.runProbe(server);
		try {
			await new ProbeMessage({ bitrate: 4_000_000, rtt: 20 }).encode(client.writer, version);
			await clock.advance(Duration.millis(100));
			expect((await ProbeMessage.decode(client.reader, version)).bitrate).toBe(1_000_000);
			expect(reset).not.toHaveBeenCalled();
			client.writer.close();
			await clock.advance(Duration.ZERO);
			expect(close).toHaveBeenCalled();
		} finally {
			client.close();
			server.close();
			pair.client.close();
			publisher.close();
			await serving;
			reset.mockRestore();
			close.mockRestore();
			installed.close();
		}
	});
}

for (const version of [Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07]) {
	test(`malformed SUBSCRIBE_UPDATE closes the session after request setup (${version})`, async () => {
		const clock = new Manual();
		const installed = clock.install();
		const pair = createMockTransportPair(ALPN_05);
		const origin = new OriginProducer();
		const broadcast = publish(origin, Path.from("test"));
		const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
		const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
		const client = await Stream.open(pair.client, { version });
		const server = await Stream.accept(pair.server, version);
		if (!server) throw new Error("publisher did not accept SUBSCRIBE");
		const close = spyOn(pair.server, "close");
		const serving = publisher.runSubscribe(
			new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "video", priority: 0 }),
			server,
		);
		try {
			await clock.advance(Duration.ZERO);
			// The complete zero-length update cannot hold its required fields.
			await client.writer.write(new Uint8Array([0]));
			await clock.advance(Duration.ZERO);
			expect(close.mock.calls.some(([info]) => info?.closeCode === SessionCode.ProtocolViolation)).toBe(true);
		} finally {
			client.close();
			server.close();
			publisher.close();
			track.finish();
			track.close();
			broadcast.close();
			origin.close();
			pair.client.close();
			await serving;
			close.mockRestore();
			installed.close();
		}
	});
}

test("lite-07 SYNCED survives an initial removal and continuous earlier-prefix churn", async () => {
	const version = Version.DRAFT_07;
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
	const removed = publish(origin, Path.from("z/removed"));
	const retained = publish(origin, Path.from("z/retained"));
	const entered = Promise.withResolvers<void>();
	const release = Promise.withResolvers<void>();
	let output!: ReadableStreamDefaultController<Uint8Array>;
	const reader = new Reader(
		new ReadableStream<Uint8Array>({
			start(controller) {
				output = controller;
			},
		}),
		undefined,
		version,
	);
	let writes = 0;
	let churnEnabled = true;
	const churn: BroadcastProducer[] = [];
	const stream = new Stream({
		version,
		readable: new ReadableStream<Uint8Array>(),
		writable: new WritableStream<Uint8Array>({
			async write(chunk) {
				if (writes++ === 0) {
					entered.resolve();
					await release.promise;
				}
				output.enqueue(new Uint8Array(chunk));
				if (churnEnabled) {
					churn.push(publish(origin, Path.from(`a/${writes}`)));
					if (churn.length > 2) churn.shift()?.close();
				}
			},
		}),
	});
	const running = publisher.runAnnounce(new AnnounceRequest(Path.empty()), stream);
	try {
		await entered.promise;
		removed.close();
		release.resolve();
		expect((await AnnounceOk.decode(reader, version)).active).toBe(0);
		const events: AnnounceBroadcast[] = [];
		for (let i = 0; i < 5; i++) {
			const event = await decodeAnnounceBroadcast(reader, version);
			events.push(event);
			if (event.status === "synced") break;
		}
		// The route, its claims, and their floor, then SYNCED.
		expect(events[0]).toMatchObject({ status: "routeStart" });
		expect(events[1]).toMatchObject({ status: "active", suffix: "z/retained" });
		expect(events.at(-2)).toMatchObject({ status: "floor", id: 0n });
		expect(events.at(-1)).toEqual({ status: "synced" });
		expect(events).not.toContainEqual(expect.objectContaining({ suffix: "z/removed" }));
	} finally {
		churnEnabled = false;
		await publisher.drain();
		await running;
		publisher.close();
		retained.close();
		for (const broadcast of churn) broadcast.close();
		origin.close();
		pair.client.close();
		pair.server.close();
	}
});

test("lite-06: a datagram request failing behind the shared writer resets rather than ends", async () => {
	const version = Version.DRAFT_06;
	const pair = createMockTransportPair(ALPN_06);
	// The legacy API's one session queue, stalled on its first write.
	let writes = 0;
	const stall = Promise.withResolvers<void>();
	Object.defineProperty(pair.server.datagrams, "writable", {
		value: new WritableStream<Uint8Array>({
			write() {
				writes++;
				return writes === 1 ? stall.promise : undefined;
			},
		}),
	});
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const first = broadcast.createDatagrams("a");
	const second = broadcast.createDatagrams("b");

	const subscribe = async (id: bigint, track: string) => {
		const client = await Stream.open(pair.client, { version });
		const server = await Stream.accept(pair.server, version);
		if (!server) throw new Error("publisher never accepted the subscribe stream");
		void publisher.runSubscribe(new Subscribe({ id, broadcast: Path.from("test"), track, priority: 0 }), server);
		expect("start" in (await decodeSubscribeResponse(client.reader, version))).toBe(true);
		return client;
	};
	const settle = async () => {
		for (let i = 0; i < 20; i++) await new Promise((resolve) => setImmediate(resolve));
	};
	const a = await subscribe(0n, "a");
	const b = await subscribe(1n, "b");
	try {
		first.append(Timestamp.unscaled(0n), Uint8Array.of(0xaa));
		while (writes === 0) await settle();
		// The second request now waits on the writer the first one holds.
		second.append(Timestamp.unscaled(0n), Uint8Array.of(0xbb));
		await settle();
		// A SUBSCRIBE_UPDATE holding only its priority fails decoding, while the stream stays open.
		await b.writer.u53(1);
		await b.writer.u8(0);
		await expect(decodeSubscribeResponse(b.reader, version)).rejects.toThrow();
	} finally {
		stall.resolve();
		a.close();
		b.close();
		publisher.close();
		origin.close();
		pair.client.close();
		pair.server.close();
	}
});

// A lite-05/06 publisher of broadcast "test", recording the datagrams it sends.
function legacyPublisher(version: Version) {
	const pair = createMockTransportPair(version === Version.DRAFT_06 ? ALPN_06 : ALPN_05);
	const sent: Uint8Array[] = [];
	Object.defineProperty(pair.server.datagrams, "writable", {
		value: new WritableStream<Uint8Array>({
			write(chunk) {
				sent.push(chunk);
			},
		}),
	});
	const origin = new OriginProducer();
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume());
	const broadcast = publish(origin, Path.from("test"));
	const streams: Stream[] = [];
	return {
		sent,
		broadcast,
		// Send a TRACK for "events" and read its TRACK_INFO.
		async track() {
			const track = await Stream.open(pair.client, { version });
			streams.push(track);
			const server = await Stream.accept(pair.server, version);
			if (!server) throw new Error("publisher never accepted the track stream");
			void publisher.runTrackInfo(new TrackMessage(Path.from("test"), "events"), server);
			await TrackInfo.decode(track.reader, version);
		},
		// SUBSCRIBE to "events" and wait for its first datagram.
		async subscribe(priority: number) {
			const subscribe = await Stream.open(pair.client, { version });
			streams.push(subscribe);
			const server = await Stream.accept(pair.server, version);
			if (!server) throw new Error("publisher never accepted the subscribe stream");
			void publisher.runSubscribe(
				new Subscribe({ id: 0n, broadcast: Path.from("test"), track: "events", priority }),
				server,
			);
			for (let i = 0; i < 20 && sent.length === 0; i++) await new Promise((resolve) => setImmediate(resolve));
		},
		close() {
			for (const stream of streams) stream.close();
			publisher.close();
			origin.close();
			pair.client.close();
			pair.server.close();
		},
	};
}

// Lite-05/06: a requester reads TRACK_INFO as the name being accepted, and its SUBSCRIBE, on a
// stream of its own, can arrive after a datagram written meanwhile. That SUBSCRIBE still sends
// it, whether the datagram track answered the TRACK or rides beside a stream track of the name,
// and the hold adds no demand of its own.
for (const version of [Version.DRAFT_05, Version.DRAFT_06]) {
	const label = version === Version.DRAFT_06 ? "lite-06" : "lite-05";
	for (const stream of [false, true]) {
		test(`${label}, stream track ${stream}: a SUBSCRIBE sends the datagrams written since its TRACK_INFO`, async () => {
			const legacy = legacyPublisher(version);
			try {
				const datagrams = legacy.broadcast.createDatagrams("events");
				if (stream) legacy.broadcast.createTrack("events", { timescale: Timescale.MILLI });
				await legacy.track();
				datagrams.append(Timestamp.unscaled(0n), Uint8Array.of(0xaa));
				await legacy.subscribe(-5);
				expect(legacy.sent.length).toBe(1);
				// Beside a stream track, only the SUBSCRIBE asks for the datagrams.
				if (stream) expect(datagrams.priority.peek()).toBe(-5);
			} finally {
				legacy.close();
			}
		});
	}

	// With two TRACKs of one name open, a SUBSCRIBE reads from the older answer, so a datagram
	// written between the two still reaches the SUBSCRIBE of the first.
	test(`${label}: a SUBSCRIBE reads from the oldest hold`, async () => {
		const legacy = legacyPublisher(version);
		try {
			const datagrams = legacy.broadcast.createDatagrams("events");
			await legacy.track();
			datagrams.append(Timestamp.unscaled(0n), Uint8Array.of(0xaa));
			await legacy.track();
			await legacy.subscribe(0);
			expect(legacy.sent.length).toBe(1);
		} finally {
			legacy.close();
		}
	});

	// A hold reads the datagram track that answered its TRACK. Once that track is replaced, a
	// SUBSCRIBE reads the replacement, even while the older TRACK stays open.
	test(`${label}: a SUBSCRIBE reads the current datagram track past an older hold`, async () => {
		const legacy = legacyPublisher(version);
		try {
			const first = legacy.broadcast.createDatagrams("events");
			await legacy.track();
			first.append(Timestamp.unscaled(0n), Uint8Array.of(0xaa));
			first.finish();
			const current = legacy.broadcast.createDatagrams("events");
			await legacy.track();
			current.append(Timestamp.unscaled(0n), Uint8Array.of(0xbb));
			await legacy.subscribe(0);
			expect(legacy.sent.length).toBe(1);
			expect(legacy.sent[0]?.at(-1)).toBe(0xbb);
		} finally {
			legacy.close();
		}
	});
}

// A lite-07 announce stream the test reads as the subscriber, and answers.
function routedAnswer(origin: OriginProducer, grant?: Signal<Grant | undefined>, prefix = Path.empty()) {
	const version = Version.DRAFT_07;
	const pair = createMockTransportPair(ALPN_07_WIP);
	const auth = grant && { grant, ready: Promise.resolve() };
	const publisher = new Publisher(pair.server, version, randomHop(), origin.consume(), auth);
	let replies!: ReadableStreamDefaultController<Uint8Array>;
	let output!: ReadableStreamDefaultController<Uint8Array>;
	const reader = new Reader(new ReadableStream<Uint8Array>({ start: (c) => (output = c) }), undefined, version);
	const stream = new Stream({
		version,
		readable: new ReadableStream<Uint8Array>({ start: (c) => (replies = c) }),
		writable: new WritableStream<Uint8Array>({ write: (chunk) => output.enqueue(new Uint8Array(chunk)) }),
	});
	const running = publisher.runAnnounce(new AnnounceRequest(prefix), stream);
	return {
		node: wireOf(origin.consume()).own.node,
		running,
		next: () => decodeAnnounceBroadcast(reader, version),
		answer: async (msg: AnnounceReply) =>
			replies.enqueue(await Writer.collect(version, (w) => encodeAnnounceReply(w, msg, version))),
		ok: () => AnnounceOk.decode(reader, version),
		// The peer finishes its replies, which ends the stream.
		finish: () => replies.close(),
		close: () => {
			publisher.close();
			pair.client.close();
			pair.server.close();
		},
	};
}

test("a lite-07 answer carries this origin's claims under its own route", async () => {
	const origin = new OriginProducer();
	const cam = publish(origin, Path.from("cam"));
	const { node, running, next, answer, ok, close } = routedAnswer(origin);
	try {
		await ok();
		// Answering a stream lengthens no path, so the seqno stays.
		expect(await next()).toEqual({
			status: "routeStart",
			node,
			advert: { seqno: 0n, metric: 0n, downOnly: false, rtt: 0n, hops: [node] },
		});
		expect(await next()).toMatchObject({ status: "active", suffix: "cam", route: 0n });
		expect(await next()).toEqual({ status: "floor", id: 0n, revision: 0n });
		expect(await next()).toEqual({ status: "synced" });

		// Each change is a batch closed by the next revision.
		const mic = publish(origin, Path.from("mic"));
		expect(await next()).toMatchObject({ status: "active", suffix: "mic", route: 0n });
		expect(await next()).toEqual({ status: "floor", id: 0n, revision: 1n });
		cam.close();
		expect(await next()).toEqual({ status: "endedId", id: 0n });
		expect(await next()).toEqual({ status: "floor", id: 0n, revision: 2n });

		// The last claim withdrawn ends the route after its batch.
		mic.close();
		expect(await next()).toEqual({ status: "endedId", id: 1n });
		expect(await next()).toEqual({ status: "floor", id: 0n, revision: 3n });
		expect(await next()).toEqual({ status: "routeEnd", id: 0n });

		// The route starts again only once the end is echoed, under the next Route ID.
		const again = publish(origin, Path.from("again"));
		await yieldNow();
		await answer({ kind: "echo", id: 0n });
		expect(await next()).toMatchObject({ status: "routeStart", node });
		expect(await next()).toMatchObject({ status: "active", suffix: "again", route: 1n });
		expect(await next()).toMatchObject({ status: "floor", id: 1n });

		// A ROUTE_REQUEST for this origin raises its seqno; one for another node changes nothing.
		await answer({ kind: "request", node: node + 1n, seqno: 20n });
		await answer({ kind: "request", node, seqno: 10n });
		expect(await next()).toEqual({
			status: "routeUpdate",
			id: 1n,
			advert: { seqno: 10n, metric: 0n, downOnly: false, rtt: 0n, hops: [node] },
		});

		// An echo of a route that is not ending is a violation.
		await answer({ kind: "echo", id: 0n });
		await expect(running).rejects.toThrow(ProtocolViolation);
		again.close();
	} finally {
		close();
		origin.close();
	}
});

// A browser forwards nothing, so every session its route went out on is a path to it: closing one
// advances the seqno. A session that never carried the route changes no path.
test("a lite-07 seqno advances only when a session carrying the route closes", async () => {
	const origin = new OriginProducer();
	const cam = publish(origin, Path.from("cam"));
	const first = routedAnswer(origin);
	const second = routedAnswer(origin);
	try {
		for (const { ok, next } of [first, second]) {
			await ok();
			expect(await next()).toMatchObject({ status: "routeStart", advert: { seqno: 0n } });
			expect(await next()).toMatchObject({ status: "active", suffix: "cam" });
			expect(await next()).toMatchObject({ status: "floor" });
			expect(await next()).toEqual({ status: "synced" });
		}

		first.finish();
		await first.running;
		expect(await second.next()).toMatchObject({ status: "routeUpdate", id: 0n, advert: { seqno: 1n } });

		// The unmatched stream shows no claim, so no route goes out on it, and its close is no bump.
		const unmatched = routedAnswer(origin, undefined, Path.from("other"));
		await unmatched.ok();
		expect(await unmatched.next()).toEqual({ status: "synced" });
		unmatched.finish();
		await unmatched.running;
		const mic = publish(origin, Path.from("mic"));
		expect(await second.next()).toMatchObject({ status: "active", suffix: "mic" });
		mic.close();
	} finally {
		cam.close();
		for (const answer of [first, second]) answer.close();
		origin.close();
	}
});

// A request for the last seqno rotates this origin's identity, which ends its route. A peer
// that keeps rotating it and never echoes holds one ended route here, not one per rotation.
test("a lite-07 route starts again only once every end is echoed", async () => {
	const origin = new OriginProducer();
	const cam = publish(origin, Path.from("cam"));
	const { node, running, next, answer, ok, close } = routedAnswer(origin);
	const own = wireOf(origin.consume()).own;
	const last = 2n ** 64n - 1n;
	try {
		await ok();
		expect(await next()).toMatchObject({ status: "routeStart", node });
		expect(await next()).toMatchObject({ status: "active", suffix: "cam", route: 0n });
		expect(await next()).toMatchObject({ status: "floor", id: 0n });
		expect(await next()).toEqual({ status: "synced" });

		await answer({ kind: "request", node, seqno: last });
		expect(await next()).toEqual({ status: "routeEnd", id: 0n });
		await yieldNow();
		expect(own.node).not.toBe(node);
		await answer({ kind: "request", node: own.node, seqno: last });
		await yieldNow();

		// Nothing under the newer identities until the end is echoed, then only the latest.
		await answer({ kind: "echo", id: 0n });
		expect(await next()).toMatchObject({ status: "routeStart", node: own.node });
		expect(await next()).toMatchObject({ status: "active", suffix: "cam", route: 1n });
		expect(await next()).toMatchObject({ status: "floor", id: 1n });
	} finally {
		cam.close();
		close();
		origin.close();
		await running.catch(() => {});
	}
});

// A floor certifies the requested prefix only while the grant covers all of it, so a grant
// change that flips that re-floors the route even when it shows the same claims.
test("a lite-07 grant change alone re-floors the route", async () => {
	const origin = new OriginProducer();
	const cam = publish(origin, Path.from("a/x"));
	const grant = new Signal<Grant | undefined>(grantOf("a/x"));
	const { running, next, ok, close } = routedAnswer(origin, grant);
	try {
		await ok();
		expect(await next()).toMatchObject({ status: "routeStart" });
		expect(await next()).toMatchObject({ status: "active", suffix: "a/x", route: 0n });
		expect(await next()).toEqual({ status: "floor", id: 0n, revision: undefined });
		expect(await next()).toEqual({ status: "synced" });

		grant.set({ ...grantOf("a/x"), publish: new Path.Patterns([Path.Pattern.subtree(Path.empty())]) });
		expect(await next()).toEqual({ status: "floor", id: 0n, revision: 0n });
		grant.set(grantOf("a/x"));
		expect(await next()).toEqual({ status: "floor", id: 0n, revision: undefined });
	} finally {
		cam.close();
		close();
		origin.close();
		await running.catch(() => {});
	}
});

// A request prefix that fits the wire alone may overflow once joined to the handle's root:
// nothing the scope allows lies there, so the floor certifies nothing rather than failing.
test("a lite-07 request beyond the scope's path limits floors uncertified", async () => {
	const origin = new OriginProducer();
	const scoped = origin.scope(Path.from("tenant"), new Path.Patterns([Path.Pattern.parse("room/**")]));
	// The claim at room covers the request, so the answer floors it.
	const room = publish(scoped, Path.from("room"));
	const prefix = Path.from(["room", ...Array.from({ length: Path.MAX_PARTS - 1 }, () => "a")].join("/"));
	const { running, next, ok, close } = routedAnswer(scoped, undefined, prefix);
	try {
		await ok();
		expect(await next()).toMatchObject({ status: "routeStart" });
		expect(await next()).toMatchObject({ status: "active", suffix: "", route: 0n });
		expect(await next()).toEqual({ status: "floor", id: 0n, revision: undefined });
		expect(await next()).toEqual({ status: "synced" });
	} finally {
		room.close();
		close();
		origin.close();
		await running.catch(() => {});
	}
});

// The publishing origin closing ends its claims and route, and finishes the stream.
test("a lite-07 answer finishes once the publishing origin closes", async () => {
	const origin = new OriginProducer();
	const cam = publish(origin, Path.from("cam"));
	const { running, next, ok, close } = routedAnswer(origin);
	try {
		await ok();
		expect(await next()).toMatchObject({ status: "routeStart" });
		expect(await next()).toMatchObject({ status: "active", suffix: "cam", route: 0n });
		expect(await next()).toMatchObject({ status: "floor", id: 0n });
		expect(await next()).toEqual({ status: "synced" });

		origin.close();
		expect(await next()).toEqual({ status: "endedId", id: 0n });
		expect(await next()).toMatchObject({ status: "floor", id: 0n });
		expect(await next()).toEqual({ status: "routeEnd", id: 0n });
		await running;
	} finally {
		cam.close();
		close();
	}
});

test("a lite-07 request naming another origin node is unroutable", async () => {
	const pair = createMockTransportPair(ALPN_07_WIP);
	const origin = new OriginProducer();
	const cam = publish(origin, Path.from("cam"));
	const publisher = new Publisher(pair.server, Version.DRAFT_07, randomHop(), origin.consume());
	const node = wireOf(origin.consume()).own.node;
	const resets = spyOn(Writer.prototype, "reset");
	const stream = () =>
		new Stream({
			version: Version.DRAFT_07,
			readable: new ReadableStream<Uint8Array>(),
			writable: new WritableStream<Uint8Array>(),
		});
	try {
		await publisher.runTrackInfo(new TrackMessage(Path.from("cam"), "video", undefined, node + 1n), stream());
		const codes = resets.mock.calls.map(([reason]) => toStreamCode(reason as Error));
		expect(codes).toContain(StreamCode.Unroutable);
	} finally {
		resets.mockRestore();
		publisher.close();
		cam.close();
		origin.close();
		pair.client.close();
		pair.server.close();
	}
});
