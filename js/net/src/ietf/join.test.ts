import { expect, jest, onTestFinished, test } from "bun:test";
import * as Connection from "../connection/index.ts";
import { GRACE } from "../ledger.ts";
import { createMockTransportPair, textFrame } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Reader, Stream, Writer } from "../stream.ts";
import { Tail } from "../tail.ts";
import { Timescale } from "../time.ts";
import * as track from "../track.ts";
import { wireOf } from "../wire.ts";
import { Fetch, FetchOk } from "./fetch.ts";
import { Join } from "./join.ts";
import { decodeFetchObject, FetchFrame, type GroupFlags, Group as GroupMessage } from "./object.ts";
import type { MessageLocation } from "./parameters.ts";
import { PublishDone, PublishDoneStatus } from "./publish.ts";
import { RequestError } from "./request.ts";
import { NativeSession } from "./session.ts";
import { Subscribe, SubscribeOk } from "./subscribe.ts";
import { Subscriber } from "./subscriber.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const VERSION = Version.DRAFT_18;
const DRAFT_22 = Version.DRAFT_22;
const ALIAS = 9n;
/** The group SUBSCRIBE_OK names as Largest in most cases below. */
const GROUP = 3;

const text = (s: string) => new TextEncoder().encode(s);
const untext = (b: Uint8Array | undefined) => (b ? new TextDecoder().decode(b) : undefined);

/** Accept the next bidi stream the subscriber opens, or undefined if none comes soon. */
async function nextStream(
	transport: WebTransport,
	wait = 200,
	version: IetfVersion = VERSION,
): Promise<Stream | undefined> {
	const reader =
		transport.incomingBidirectionalStreams.getReader() as ReadableStreamDefaultReader<WebTransportBidirectionalStream>;
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		const next = await Promise.race([
			reader.read(),
			new Promise<undefined>((resolve) => {
				timer = setTimeout(() => resolve(undefined), wait);
			}),
		]);
		if (!next || next.done) return undefined;
		return new Stream({ readable: next.value.readable, writable: next.value.writable, version });
	} finally {
		clearTimeout(timer);
		reader.releaseLock();
	}
}

/** A subscriber with one track, and the SUBSCRIBE it sent, still unanswered. */
async function subscribed(version: IetfVersion = VERSION) {
	const pair = createMockTransportPair(version === VERSION ? ALPN.DRAFT_18 : ALPN.DRAFT_22);
	const session = new NativeSession(pair.server, version, true);
	const subscriber = new Subscriber({ session });
	const consumer = subscriber.consume(Path.from("room")).track("video").subscribe();
	onTestFinished(() => {
		consumer.close();
		session.close();
	});

	const peer = await nextStream(pair.client, 200, version);
	if (!peer) throw new Error("the subscriber never subscribed");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	const subscribe = await Subscribe.decode(peer.reader, version);
	const answer = async (largest: MessageLocation | null) => {
		await peer.writer.u53(SubscribeOk.id);
		await new SubscribeOk({ trackAlias: ALIAS, largest: largest ?? undefined }).encode(peer.writer, version);
	};
	return { subscriber, consumer, subscribe, peer, answer, client: pair.client };
}

/**
 * A draft-18 subscriber with one track answered with Largest `largest`, and the joining
 * FETCH it sent in response, still unanswered.
 */
async function join(largest: MessageLocation | null = { groupId: BigInt(GROUP), objectId: 1n }) {
	const subscription = await subscribed();
	await subscription.answer(largest);

	const fetching = await nextStream(subscription.client);
	let fetch: Fetch | undefined;
	if (fetching) {
		expect(await fetching.reader.u53()).toBe(Fetch.id);
		fetch = await Fetch.decode(fetching.reader, VERSION);
	}
	return { ...subscription, fetching, fetch };
}

async function accept(fetching: Stream | undefined) {
	if (!fetching) throw new Error("no joining FETCH");
	await fetching.writer.u53(FetchOk.id);
	await new FetchOk({ endOfTrack: false, endLocation: { group: BigInt(GROUP), object: 2n } }).encode(
		fetching.writer,
		VERSION,
	);
}

async function refuse(fetching: Stream | undefined) {
	if (!fetching) throw new Error("no joining FETCH");
	await fetching.writer.u53(RequestError.id);
	await new RequestError({ errorCode: 0x4, reasonPhrase: "not here" }).encode(fetching.writer, VERSION);
}

/** A fetch stream after its type: the header naming `requestId`, then `payloads` from object 0. */
async function head(requestId: bigint, payloads: string[], group = GROUP, version: IetfVersion = VERSION) {
	const bytes = await Writer.collect(version, async (w) => {
		await w.u62(requestId);
		for (const [object, payload] of payloads.entries()) {
			await new FetchFrame({ payload: text(payload) }).encode(
				w,
				{ group, object, first: object === 0 },
				undefined,
				version,
			);
		}
	});
	return new Reader(undefined, bytes, version);
}

function flags(firstObject: boolean): GroupFlags {
	return {
		hasExtensions: false,
		hasSubgroup: false,
		hasSubgroupObject: false,
		hasEnd: true,
		hasPriority: true,
		firstObject,
	};
}

/** A live subgroup stream for `group` starting at Object ID `start`. */
function liveStream(
	group: number,
	start: number,
	payloads: string[],
	version: IetfVersion = VERSION,
): [GroupMessage, Reader] {
	const bytes: number[] = [];
	for (const [i, payload] of payloads.entries()) {
		const p = text(payload);
		// Every field here is under 64, so each is a one-byte varint.
		bytes.push(i === 0 ? start : 0, p.byteLength, ...p);
	}
	const header = new GroupMessage({
		trackAlias: ALIAS,
		groupId: group,
		subGroupId: 0,
		publisherPriority: 0,
		flags: flags(start === 0),
	});
	return [header, new Reader(undefined, new Uint8Array(bytes), version)];
}

async function frames(group: { readFrame(): Promise<{ payload: Uint8Array } | undefined> } | undefined) {
	const out: (string | undefined)[] = [];
	for (;;) {
		const frame = await group?.readFrame();
		if (!frame) return out;
		out.push(untext(frame.payload));
	}
}

test("a draft-18 join subscribes at Largest Object and fetches Largest's group", async () => {
	const { subscribe, fetch } = await join();
	expect(subscribe.filter).toEqual({ kind: "nextObject" });
	expect(fetch?.type).toEqual({ kind: "relativeJoining", subscription: subscribe.requestId, offset: 0n });
	expect(fetch?.groupOrder).toBe("ascending");
	expect(fetch?.requestId).not.toBe(subscribe.requestId);
});

test("an empty track sends no joining FETCH", async () => {
	const { fetching } = await join(null);
	expect(fetching).toBeUndefined();
});

test("a head that lands before its tail is continued by it", async () => {
	const { subscriber, consumer, fetching, fetch } = await join();
	if (!fetch) throw new Error("no fetch");
	await accept(fetching);

	await subscriber.handleFetch(await head(fetch.requestId, ["h0", "h1"]));
	await subscriber.handleGroup(...liveStream(GROUP, 2, ["t2", "t3"]));

	const group = await consumer.ordered().nextGroup();
	expect(group?.sequence).toBe(GROUP);
	expect(await frames(group)).toEqual(["h0", "h1", "t2", "t3"]);
});

test("a tail that lands before its head waits for it", async () => {
	const { subscriber, consumer, fetching, fetch } = await join();
	if (!fetch) throw new Error("no fetch");

	const tail = subscriber.handleGroup(...liveStream(GROUP, 2, ["t2"]));
	await accept(fetching);
	await subscriber.handleFetch(await head(fetch.requestId, ["h0", "h1"]));
	await tail;

	const group = await consumer.ordered().nextGroup();
	expect(await frames(group)).toEqual(["h0", "h1", "t2"]);
});

test("a catalog written once is readable from the head alone", async () => {
	const { subscriber, consumer, fetching, fetch } = await join({ groupId: 0n, objectId: 0n });
	if (!fetch) throw new Error("no fetch");
	await accept(fetching);

	// No live stream ever comes: the group was written before the subscription.
	await subscriber.handleFetch(await head(fetch.requestId, ['{"video":{}}'], 0));

	const group = await consumer.ordered().nextGroup();
	expect(group?.sequence).toBe(0);
	expect(untext((await group?.readFrame())?.payload)).toBe('{"video":{}}');
});

test("an empty tail finishes a group the head already holds whole", async () => {
	const { subscriber, consumer, fetching, fetch } = await join();
	if (!fetch) throw new Error("no fetch");
	await accept(fetching);

	await subscriber.handleFetch(await head(fetch.requestId, ["h0", "h1"]));
	const [header] = liveStream(GROUP, 2, []);
	header.flags = flags(false);
	await subscriber.handleGroup(header, new Reader(undefined, new Uint8Array(), VERSION));

	const group = await consumer.ordered().nextGroup();
	expect(await frames(group)).toEqual(["h0", "h1"]);
	expect(await group?.closed).toBeNull();
});

for (const outcome of ["a refused joining FETCH", "an empty joining FETCH"] as const) {
	test(`${outcome} costs only the group's head`, async () => {
		const { subscriber, consumer, fetching, fetch, client } = await join();
		if (!fetch) throw new Error("no fetch");
		if (outcome === "an empty joining FETCH") {
			await accept(fetching);
			await subscriber.handleFetch(await head(fetch.requestId, []));
		} else {
			await refuse(fetching);
		}
		// No standalone FETCH follows: a relay that refuses the join has a bug of its own.
		expect(await nextStream(client)).toBeUndefined();

		// The mid-group tail has nothing to stitch onto, so it is dropped; the next group plays.
		await subscriber.handleGroup(...liveStream(GROUP, 2, ["t2"]));
		await subscriber.handleGroup(...liveStream(GROUP + 1, 0, ["n0"]));

		const group = await consumer.ordered().nextGroup();
		expect(group?.sequence).toBe(GROUP + 1);
		expect(await frames(group)).toEqual(["n0"]);
	});
}

test("a tail parked on a refused joining FETCH is dropped", async () => {
	const { subscriber, consumer, fetching } = await join();

	const tail = subscriber.handleGroup(...liveStream(GROUP, 2, ["t2"]));
	await refuse(fetching);
	await tail;
	await subscriber.handleGroup(...liveStream(GROUP + 1, 0, ["n0"]));

	const group = await consumer.ordered().nextGroup();
	expect(group?.sequence).toBe(GROUP + 1);
});

test("a joining FETCH has no deadline of its own", async () => {
	jest.useFakeTimers();
	onTestFinished(() => jest.useRealTimers());
	const { subscriber, consumer, fetching, fetch, client } = await join();
	if (!fetch) throw new Error("no fetch");

	const tail = subscriber.handleGroup(...liveStream(GROUP, 2, ["t2"]));
	// Well past any request deadline: the FETCH is still out, and nothing else was sent.
	jest.advanceTimersByTime(60_000);
	jest.useRealTimers();
	expect(await nextStream(client)).toBeUndefined();

	await accept(fetching);
	await subscriber.handleFetch(await head(fetch.requestId, ["h0", "h1"]));
	await tail;

	const group = await consumer.ordered().nextGroup();
	expect(group?.sequence).toBe(GROUP);
	expect(await frames(group)).toEqual(["h0", "h1", "t2"]);
});

test("a joining FETCH reset after FETCH_OK costs only the head", async () => {
	const { subscriber, consumer, fetching, peer } = await join();
	const started = performance.now();

	const tail = subscriber.handleGroup(...liveStream(GROUP, 2, ["t2"]));
	await accept(fetching);
	// No fetch stream ever comes.
	fetching?.writer.reset(new Error("gone"));
	await tail;
	await subscriber.handleGroup(...liveStream(GROUP + 1, 0, ["n0"]));
	await peer.writer.u53(PublishDone.id);
	const done = new PublishDone({ statusCode: PublishDoneStatus.TRACK_ENDED, streamCount: 2n, reasonPhrase: "done" });
	await done.encode(peer.writer, VERSION);

	// The parked tail was dropped and nothing holds the end open.
	const group = await consumer.recvGroup();
	expect(group?.sequence).toBe(GROUP + 1);
	expect(await consumer.recvGroup()).toBeUndefined();
	expect(await consumer.closed).toBeNull();
	expect(performance.now() - started).toBeLessThan(GRACE);
});

test("SUBSCRIPTION_ENDED cancels a head nothing claimed", async () => {
	const { subscriber, consumer, fetching, fetch, peer } = await join();
	if (!fetch) throw new Error("no fetch");
	await accept(fetching);
	await subscriber.handleFetch(await head(fetch.requestId, ["h0", "h1"]));
	const group = await consumer.ordered().nextGroup();
	expect(group?.sequence).toBe(GROUP);

	// The group may run past the head upstream, so it must not read as whole.
	await subscriber.handleGroup(...liveStream(GROUP + 1, 0, ["n0"]));
	await peer.writer.u53(PublishDone.id);
	const done = new PublishDone({
		statusCode: PublishDoneStatus.SUBSCRIPTION_ENDED,
		streamCount: 1n,
		reasonPhrase: "done",
	});
	await done.encode(peer.writer, VERSION);
	expect(await consumer.closed).toBeInstanceOf(Error);
	await expect(frames(group)).rejects.toThrow();
});

test("a whole group that precedes the head wins the sequence", async () => {
	const { subscriber, consumer, fetching, fetch } = await join();
	if (!fetch) throw new Error("no fetch");
	await accept(fetching);

	await subscriber.handleGroup(...liveStream(GROUP, 0, ["w0", "w1", "w2"]));
	await subscriber.handleFetch(await head(fetch.requestId, ["h0", "h1"]));

	const group = await consumer.ordered().nextGroup();
	expect(await frames(group)).toEqual(["w0", "w1", "w2"]);
});

test("a whole group for a headed sequence is refused, with its head", async () => {
	const { subscriber, consumer, fetching, fetch } = await join();
	if (!fetch) throw new Error("no fetch");
	await accept(fetching);

	await subscriber.handleFetch(await head(fetch.requestId, ["h0", "h1"]));
	await subscriber.handleGroup(...liveStream(GROUP, 0, ["w0"]));

	const group = await consumer.ordered().nextGroup();
	expect(group?.sequence).toBe(GROUP);
	expect(await group?.closed).toBeInstanceOf(Error);
});

test("a head that does not meet its tail is refused", async () => {
	const { subscriber, consumer, fetching, fetch } = await join();
	if (!fetch) throw new Error("no fetch");
	await accept(fetching);

	await subscriber.handleFetch(await head(fetch.requestId, ["h0", "h1"]));
	// Object 2 is missing.
	await subscriber.handleGroup(...liveStream(GROUP, 3, ["t3"]));

	const group = await consumer.ordered().nextGroup();
	expect(await group?.closed).toBeInstanceOf(Error);
});

/** A head whose objects after the first spell Object ID Delta `delta` (flag 0x04, no Group ID). */
async function deltaHead(requestId: bigint, payloads: string[], delta: number) {
	const bytes = await Writer.collect(VERSION, async (w) => {
		await w.u62(requestId);
		for (const [object, payload] of payloads.entries()) {
			if (object === 0) {
				await new FetchFrame({ payload: text(payload) }).encode(
					w,
					{ group: GROUP, object, first: true },
					undefined,
					VERSION,
				);
				continue;
			}
			const p = text(payload);
			await w.write(new Uint8Array([0x04, delta, p.byteLength, ...p]));
		}
	});
	return new Reader(undefined, bytes, VERSION);
}

test("a head that spells Object ID Delta 1 is contiguous", async () => {
	const { subscriber, consumer, fetching, fetch } = await join();
	if (!fetch) throw new Error("no fetch");
	await accept(fetching);

	await subscriber.handleFetch(await deltaHead(fetch.requestId, ["h0", "h1"], 1));
	await subscriber.handleGroup(...liveStream(GROUP, 2, ["t2"]));

	const group = await consumer.ordered().nextGroup();
	expect(await frames(group)).toEqual(["h0", "h1", "t2"]);
});

test("a head that repeats an Object ID is dropped", async () => {
	const { subscriber, consumer, fetching, fetch } = await join();
	if (!fetch) throw new Error("no fetch");
	await accept(fetching);

	await subscriber.handleFetch(await deltaHead(fetch.requestId, ["h0", "h0"], 0));

	const group = await consumer.ordered().nextGroup();
	expect(await group?.closed).toBeInstanceOf(Error);
});

test("a tail that is only an End of Group finishes the head", async () => {
	const { subscriber, consumer, fetching, fetch } = await join();
	if (!fetch) throw new Error("no fetch");
	await accept(fetching);

	await subscriber.handleFetch(await head(fetch.requestId, ["h0", "h1"]));
	const [header] = liveStream(GROUP, 2, []);
	header.flags = flags(false);
	// Object ID 2, an empty payload, then status End of Group.
	await subscriber.handleGroup(header, new Reader(undefined, new Uint8Array([2, 0, 3]), VERSION));

	const group = await consumer.ordered().nextGroup();
	expect(await frames(group)).toEqual(["h0", "h1"]);
	expect(await group?.closed).toBeNull();
});

test("a joining FETCH spanning another group drops the head", async () => {
	const { subscriber, consumer, fetching, fetch } = await join();
	if (!fetch) throw new Error("no fetch");
	await accept(fetching);

	const bytes = await Writer.collect(VERSION, async (w) => {
		await w.u62(fetch.requestId);
		await new FetchFrame({ payload: text("h0") }).encode(
			w,
			{ group: GROUP, object: 0, first: true },
			undefined,
			VERSION,
		);
		await new FetchFrame({ payload: text("x0") }).encode(
			w,
			{ group: GROUP + 1, object: 0, first: false, previousGroup: GROUP },
			undefined,
			VERSION,
		);
	});
	await subscriber.handleFetch(new Reader(undefined, bytes, VERSION));

	const group = await consumer.ordered().nextGroup();
	expect(await group?.closed).toBeInstanceOf(Error);
});

test("a draft-22 join subscribes at Next Object with a StartGroup=1 fill, and sends no FETCH", async () => {
	const { subscribe, answer, client } = await subscribed(DRAFT_22);
	expect(subscribe.filter).toEqual({ kind: "nextObject" });
	expect(subscribe.fill).toEqual({ filter: { kind: "relative", groups: 1n }, rangeFilters: false });
	await answer({ groupId: BigInt(GROUP), objectId: 1n });
	expect(await nextStream(client, 200, DRAFT_22)).toBeUndefined();
});

test("a draft-22 fill and its tail stitch into one group", async () => {
	const { subscriber, consumer, subscribe, answer } = await subscribed(DRAFT_22);
	await answer({ groupId: BigInt(GROUP), objectId: 1n });

	const tail = subscriber.handleGroup(...liveStream(GROUP, 2, ["t2"], DRAFT_22));
	// The fill names the SUBSCRIBE's own request.
	await subscriber.handleFetch(await head(subscribe.requestId, ["h0", "h1"], GROUP, DRAFT_22));
	await tail;

	const group = await consumer.ordered().nextGroup();
	expect(group?.sequence).toBe(GROUP);
	expect(await frames(group)).toEqual(["h0", "h1", "t2"]);
});

test("a draft-22 fill that overtakes SUBSCRIBE_OK waits for it", async () => {
	const { subscriber, consumer, subscribe, answer } = await subscribed(DRAFT_22);
	const fill = subscriber.handleFetch(await head(subscribe.requestId, ["h0", "h1"], GROUP, DRAFT_22));
	await answer({ groupId: BigInt(GROUP), objectId: 1n });
	await fill;
	await subscriber.handleGroup(...liveStream(GROUP, 2, ["t2"], DRAFT_22));

	const group = await consumer.ordered().nextGroup();
	expect(await frames(group)).toEqual(["h0", "h1", "t2"]);
});

test("a draft-22 fill counts toward PUBLISH_DONE's Stream Count", async () => {
	const { subscriber, consumer, subscribe, peer, answer } = await subscribed(DRAFT_22);
	await answer({ groupId: 0n, objectId: 0n });
	const started = performance.now();
	await subscriber.handleFetch(await head(subscribe.requestId, ["catalog"], 0, DRAFT_22));
	await peer.writer.u53(PublishDone.id);
	const done = new PublishDone({ statusCode: PublishDoneStatus.TRACK_ENDED, streamCount: 1n, reasonPhrase: "done" });
	await done.encode(peer.writer, DRAFT_22);

	// The fill was the one stream counted, so the head is the whole group and the track ends.
	const group = await consumer.recvGroup();
	expect(group?.sequence).toBe(0);
	expect(await frames(group)).toEqual(["catalog"]);
	expect(await consumer.recvGroup()).toBeUndefined();
	expect(await consumer.closed).toBeNull();
	expect(performance.now() - started).toBeLessThan(GRACE);
});

test("a clean end finishes a head nothing claimed", () => {
	const t = new Tail();
	const j = new Join(0, t);
	const producer = new track.Producer("catalog").createGroup(0);
	expect(j.open(() => producer)).toBe(producer);
	producer.writeFrame({ payload: text("c") });
	j.wrote();
	j.finish();
	expect(j.outstanding).toBe(true);
	j.end();
	expect(producer.closed.peek()).toBeNull();
	expect(j.outstanding).toBe(false);
});

test("fetch objects decode as FetchFrame wrote them", async () => {
	const bytes = await Writer.collect(VERSION, async (w) => {
		await new FetchFrame({ payload: text("a") }).encode(
			w,
			{ group: 5, object: 0, first: true },
			undefined,
			VERSION,
		);
		await new FetchFrame({ payload: new Uint8Array() }).encode(
			w,
			{ group: 5, object: 1, first: false },
			undefined,
			VERSION,
		);
	});
	const reader = new Reader(undefined, bytes, VERSION);
	const first = await reader.decode((c) => decodeFetchObject(c, undefined));
	expect(first).toMatchObject({ kind: "object", group: 5, object: 0, priority: 0, subgroup: "zero" });
	const second = await reader.decode((c) => decodeFetchObject(c, undefined));
	expect(second).toMatchObject({ kind: "object", group: undefined, object: undefined, payload: new Uint8Array() });
});

// End to end against the JS publisher, which answers the joining FETCH from its cache and
// serves a fill split from the subscription at Largest, as a strict relay does.
async function publishing(version: IetfVersion) {
	const origin = new OriginProducer();
	const pair = createMockTransportPair(version === VERSION ? ALPN.DRAFT_18 : ALPN.DRAFT_22);
	const url = new URL("https://localhost:4443/test");
	const [client, server] = await Promise.all([
		Connection.connect({ url, transport: pair.client }),
		Connection.accept({ transport: pair.server, url, publish: origin.consume() }),
	]);
	onTestFinished(() => {
		client.abort();
		server.abort();
	});

	const publication = origin.createBroadcast(Path.from("room"));
	publication.announce();
	const broadcast = publication.broadcast;
	// Written once, before anyone subscribed, and left open: only the head can deliver it.
	const catalog = broadcast.createTrack("catalog.json", { timescale: Timescale.MILLI });
	catalog.appendGroup().writeFrame(textFrame("catalog"));
	const video = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const group = video.appendGroup();
	group.writeFrame(textFrame("v0"));
	group.writeFrame(textFrame("v1"));

	const sent = jest.spyOn(Fetch.prototype, "encode");
	onTestFinished(() => sent.mockRestore());
	return { remote: wireOf(client).consume(Path.from("room")), group, sent };
}

for (const version of [VERSION, DRAFT_22]) {
	const draft = version === VERSION ? "draft 18" : "draft 22";
	test(`${draft}: a late JS subscriber gets a JS publisher's current groups from object 0`, async () => {
		const { remote, group, sent } = await publishing(version);
		const catalogGroup = await remote.track("catalog.json").subscribe().recvGroup();
		expect(catalogGroup?.sequence).toBe(0);
		expect(untext((await catalogGroup?.readFrame())?.payload)).toBe("catalog");

		const videoGroup = await remote.track("video").subscribe().recvGroup();
		expect(videoGroup?.sequence).toBe(0);
		group.writeFrame(textFrame("v2"));
		group.finish();
		expect(await frames(videoGroup)).toEqual(["v0", "v1", "v2"]);
		// A joining FETCH per track on draft 18; draft 22 asks for the head with a fill.
		expect(sent.mock.calls).toHaveLength(version === VERSION ? 2 : 0);
	});
}
