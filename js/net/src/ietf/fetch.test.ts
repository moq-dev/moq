import { expect, test } from "bun:test";
import { Manual, Milli, Nano } from "@moq/clock";
import { Signal } from "@moq/signals";
import type { Grant } from "../auth.ts";
import { error } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Reader, Stream, Writer } from "../stream.ts";
import { Timescale, Timestamp } from "../time.ts";
import type { Order, Producer as TrackProducer } from "../track.ts";
import { wireOf } from "../wire.ts";
import { Fetch, FetchHeader, FetchOk, type FetchType, type Location } from "./fetch.ts";
import type * as Filter from "./filter.ts";
import { Group as GroupMessage } from "./object.ts";
import { Publisher } from "./publisher.ts";
import { RequestError } from "./request.ts";
import { NativeSession } from "./session.ts";
import { Subscribe, SubscribeOk } from "./subscribe.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const VERSIONS = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const;
const D18 = Version.DRAFT_18;
const D20 = Version.DRAFT_20;
const D22 = Version.DRAFT_22;
const ALPNS: Record<IetfVersion, string> = { [D18]: ALPN.DRAFT_18, [D20]: ALPN.DRAFT_20, [D22]: ALPN.DRAFT_22 };

const SUBSCRIBE_ID = 7n;
const FETCH_ID = 9n;

const TIMEOUT = 0x2;
const NOT_SUPPORTED = 0x3;
const UNAUTHORIZED = 0x1;
const DOES_NOT_EXIST = 0x10;
const INVALID_RANGE = 0x11;
const INVALID_JOINING_REQUEST_ID = 0x32;

/** A publisher serving the `room` broadcast, with its `video` track and the client's uni streams. */
function serve(version: IetfVersion, grant?: Signal<Grant | undefined>) {
	const pair = createMockTransportPair(ALPNS[version]);
	const origin = new OriginProducer();
	const pub = new Publisher({
		quic: pair.server,
		session: new NativeSession(pair.server, version, false),
		publish: origin.consume(),
		requiresSolicitation: false,
		grant,
	});
	const publication = origin.createBroadcast(Path.from("room"));
	publication.announce();
	const track = publication.broadcast.createTrack("video", { timescale: Timescale.MILLI });
	const uni = pair.client.incomingUnidirectionalStreams.getReader() as ReadableStreamDefaultReader<
		ReadableStream<Uint8Array>
	>;
	return {
		pair,
		pub,
		broadcast: publication.broadcast,
		track,
		uni,
		version,
		[Symbol.dispose]() {
			uni.releaseLock();
			origin.close();
		},
	};
}

type Served = ReturnType<typeof serve>;

/** Write `payloads` into group `sequence`, finishing it unless told otherwise. */
function writeGroup(track: TrackProducer, sequence: number, payloads: string[], finish = true) {
	const group = track.createGroup(sequence);
	for (const [object, payload] of payloads.entries()) {
		group.writeFrame({
			payload: new TextEncoder().encode(payload),
			timestamp: new Timestamp(object, Timescale.MILLI),
		});
	}
	if (finish) group.finish();
	return group;
}

/** Groups `0..count`, each holding `g-0` and `g-1`, skipping `hole`. */
function publishPairs(track: TrackProducer, count: number, hole?: number) {
	for (let sequence = 0; sequence < count; sequence++) {
		if (sequence !== hole) writeGroup(track, sequence, [`${sequence}-0`, `${sequence}-1`]);
	}
}

/** The `[group, object, payload]` a run of two-object groups holds. */
function pairs(groups: number[]): [number, number, string][] {
	return groups.flatMap((group) =>
		[0, 1].map((object): [number, number, string] => [group, object, `${group}-${object}`]),
	);
}

/**
 * The range `[start, end)` as each draft spells it: a standalone FETCH through draft-18, End
 * Object 0 for the whole End Group, and an inclusive LOCATION_FILTER from draft-22.
 */
function range(version: IetfVersion, start: Location, end: Location, trackName = "video"): FetchType {
	if (version === D18) return { kind: "standalone", trackNamespace: Path.from("room"), trackName, start, end };
	const endObject = end.object === 0n ? undefined : end.object - 1n;
	const filter: Filter.Filter = {
		kind: "absolute",
		startGroup: start.group,
		startObject: start.object,
		endGroup: end.group,
		endObject,
	};
	return filtered(filter, trackName);
}

function filtered(filter: Filter.Filter, trackName = "video"): FetchType {
	return { kind: "filtered", trackNamespace: Path.from("room"), trackName, filter };
}

/** A FETCH_OK End Location naming `last` as the last object: one past it before draft-22. */
function endAt(version: IetfVersion, group: number, last: number): Location {
	return { group: BigInt(group), object: BigInt(version === D18 ? last + 1 : last) };
}

function at(group: number, object: number): Location {
	return { group: BigInt(group), object: BigInt(object) };
}

/** A manual clock installed until disposed, so no wait can lapse on its own. */
function manual(): Manual & Disposable {
	const clock = new Manual();
	const installed = clock.install();
	return Object.assign(clock, { [Symbol.dispose]: () => installed.close() });
}

function ms(value: number): Nano {
	return Nano.fromMilli(Milli(value));
}

/** Let every pending promise chain run. */
async function flush() {
	for (let i = 0; i < 20; i++) await new Promise((resolve) => setImmediate(resolve));
}

/** What a FETCH was answered with. */
type Answer =
	| { refused: number }
	| {
			ok: FetchOk;
			/** The fetch stream's own bytes, after its type. */
			raw: Uint8Array;
			requestId: bigint;
			objects: [number, number, string][];
	  };

/** A request stream the test writes the requester's side of and reads the answer from. */
function requestStream(version: IetfVersion) {
	let input!: ReadableStreamDefaultController<Uint8Array>;
	const output = new TransformStream<Uint8Array, Uint8Array>();
	const stream = new Stream({
		readable: new ReadableStream<Uint8Array>({
			start(c) {
				input = c;
			},
		}),
		writable: output.writable,
		version,
	});
	return {
		stream,
		answer: new Reader(output.readable, undefined, version),
		/** End the request as `version` cancels one: by FIN through draft 18, and by a reset after. */
		withdraw: () => (version === D18 ? input.close() : input.error(new Error("request cancelled"))),
	};
}

/** Send `msg` and read the answer, along with the fetch stream that follows a FETCH_OK. */
async function fetch(fx: Served, msg: Fetch): Promise<Answer> {
	const request = requestStream(fx.version);
	const task = fx.pub.runFetch(msg, request.stream);
	try {
		return await answer(fx, request.answer, msg.groupOrder ?? "ascending");
	} finally {
		await task;
	}
}

async function answer(fx: Served, wire: Reader, order: Order): Promise<Answer> {
	const id = await wire.u53();
	if (id === RequestError.id) {
		const refused = (await RequestError.decode(wire, fx.version)).errorCode;
		expect(await wire.done()).toBe(true);
		return { refused };
	}
	expect(id).toBe(FetchOk.id);
	const ok = await FetchOk.decode(wire, fx.version);
	expect(await wire.done()).toBe(true);

	const next = await fx.uni.read();
	if (next.done) throw new Error("no fetch stream");
	const bytes = new Uint8Array(await new Response(next.value).arrayBuffer());
	const reader = new Reader(undefined, bytes, fx.version);
	expect(await reader.u53()).toBe(FetchHeader.type);
	const requestId = (await FetchHeader.decode(reader, fx.version)).requestId;
	return { ok, raw: bytes, requestId, objects: await readObjects(reader, order) };
}

/**
 * Decode a fetch stream's objects as `[group, object, payload]`, its groups in `order`, and
 * each End of Range as `[group, object, "end <reason in hex>"]`.
 */
async function readObjects(reader: Reader, order: Order): Promise<[number, number, string][]> {
	const objects: [number, number, string][] = [];
	let group = 0;
	let object = 0;
	while (!(await reader.done())) {
		const flags = await reader.u53();
		// Absolute ids, and nothing after them.
		if (flags >= 0x80) {
			group = Number(await reader.u62());
			object = Number(await reader.u62());
			objects.push([group, object, `end ${flags.toString(16)}`]);
			continue;
		}
		if (flags & 0x08) {
			const value = await reader.u53();
			group = objects.length === 0 ? value : order === "ascending" ? group + value + 1 : group - value - 1;
		}
		object = flags & 0x04 ? await reader.u53() : object + 1;
		if (flags & 0x10) await reader.u8();
		if (flags & 0x20) await reader.read(await reader.u53());
		const payload = new TextDecoder().decode(await reader.read(await reader.u53()));
		objects.push([group, object, payload]);
	}
	return objects;
}

function served(answer: Answer) {
	if ("refused" in answer) throw new Error(`refused with ${answer.refused}`);
	return answer;
}

function refused(answer: Answer): number {
	if (!("refused" in answer)) throw new Error("answered with FETCH_OK");
	return answer.refused;
}

test("FETCH and FETCH_OK round trip on every draft", async () => {
	const encode = async (msg: Fetch | FetchOk, version: IetfVersion) => {
		const chunks: Uint8Array[] = [];
		const writer = new Writer(
			new WritableStream({ write: (chunk) => void chunks.push(new Uint8Array(chunk)) }),
			version,
		);
		await msg.encode(writer, version);
		writer.close();
		await writer.closed;
		return new Reader(undefined, Uint8Array.from(chunks.flatMap((chunk) => [...chunk])), version);
	};
	const types: [IetfVersion, FetchType][] = [
		[D18, range(D18, at(1, 2), at(3, 0))],
		[D18, { kind: "relativeJoining", subscription: 4n, offset: 1n }],
		[D18, { kind: "absoluteJoining", subscription: 4n, group: 300n }],
		[D22, filtered({ kind: "nextObject" })],
		[D22, range(D22, at(1, 2), at(3, 5))],
	];
	for (const [version, type] of types) {
		const msg = new Fetch({
			requestId: FETCH_ID,
			type,
			subscriberPriority: 3,
			groupOrder: "descending",
			fillTimeout: Milli(250),
		});
		const decoded = await Fetch.decode(await encode(msg, version), version);
		expect(decoded).toEqual(msg);

		const ok = new FetchOk({ endOfTrack: true, endLocation: at(5, 2), properties: { timescale: Timescale.MILLI } });
		expect(await FetchOk.decode(await encode(ok, version), version)).toEqual(ok);
	}
	// Draft-22 has no Fetch Type tag, so the tagged forms cannot be written there, nor the
	// filtered one before it.
	await expect(encode(new Fetch({ requestId: 1n, type: range(D18, at(0, 0), at(0, 0)) }), D22)).rejects.toThrow();
	await expect(encode(new Fetch({ requestId: 1n, type: filtered({ kind: "unfiltered" }) }), D18)).rejects.toThrow();
});

test("a FETCH of one whole group", async () => {
	for (const version of VERSIONS) {
		using fx = serve(version);
		publishPairs(fx.track, 5);
		const answer = served(
			await fetch(fx, new Fetch({ requestId: FETCH_ID, type: range(version, at(2, 0), at(2, 0)) })),
		);
		// A whole-group answer before draft-22 keeps Object 0 rather than claiming the next group.
		expect(answer.ok.endLocation).toEqual(version === D18 ? at(2, 0) : at(2, 1));
		expect(answer.ok.endOfTrack).toBe(false);
		expect(answer.ok.properties.timescale).toBe(Timescale.MILLI);
		expect(answer.requestId).toBe(FETCH_ID);
		expect(answer.objects).toEqual(pairs([2]));
	}
});

test("a FETCH stops at its end object", async () => {
	for (const version of VERSIONS) {
		using fx = serve(version);
		publishPairs(fx.track, 5);
		const answer = served(
			await fetch(fx, new Fetch({ requestId: FETCH_ID, type: range(version, at(2, 0), at(2, 1)) })),
		);
		expect(answer.ok.endLocation).toEqual(endAt(version, 2, 0));
		expect(answer.objects).toEqual(pairs([2]).slice(0, 1));
	}
});

test("a FETCH spanning groups is refused before FETCH_OK", async () => {
	for (const version of VERSIONS) {
		for (const order of ["ascending", "descending"] as const) {
			using fx = serve(version);
			publishPairs(fx.track, 5, 2);
			for (const end of [3, 9]) {
				const type = range(version, at(0, 1), at(end, 0));
				expect(refused(await fetch(fx, new Fetch({ requestId: FETCH_ID, type, groupOrder: order })))).toBe(
					NOT_SUPPORTED,
				);
			}
		}
	}
});

// Only a finished track's final object ends it, wherever the range starts or ends.
test("a FETCH through the final object reports the end of track", async () => {
	for (const version of VERSIONS) {
		using fx = serve(version);
		publishPairs(fx.track, 5);
		fx.track.finish();

		const last = served(
			await fetch(fx, new Fetch({ requestId: FETCH_ID, type: range(version, at(4, 1), at(4, 0)) })),
		);
		expect(last.ok.endLocation).toEqual(version === D18 ? at(4, 0) : at(4, 1));
		expect(last.ok.endOfTrack).toBe(true);
		expect(last.objects).toEqual(pairs([4]).slice(1));

		// A shorter prefix must not claim the track ends there.
		const prefix = served(
			await fetch(fx, new Fetch({ requestId: FETCH_ID, type: range(version, at(4, 0), at(4, 1)) })),
		);
		expect(prefix.ok.endOfTrack).toBe(false);
	}
});

// The Largest Object's group is read whole, so an answer reaching it waits for it to finish,
// and never claims a live track ends.
test("a FETCH waits for the Largest Object's open group", async () => {
	for (const version of VERSIONS) {
		using fx = serve(version);
		publishPairs(fx.track, 4);
		const live = writeGroup(fx.track, 4, ["4-0"], false);
		let answered = false;
		const answering = fetch(fx, new Fetch({ requestId: FETCH_ID, type: range(version, at(4, 0), at(4, 0)) }));
		void answering.finally(() => {
			answered = true;
		});
		await flush();
		expect(answered).toBe(false);
		live.writeFrame({ payload: new TextEncoder().encode("4-1"), timestamp: new Timestamp(1, Timescale.MILLI) });
		live.finish();
		const answer = served(await answering);
		expect(answer.ok.endLocation).toEqual(version === D18 ? at(4, 0) : at(4, 1));
		expect(answer.ok.endOfTrack).toBe(false);
		expect(answer.objects).toEqual(pairs([4]));
	}
});

test("a FETCH the cache cannot answer is refused before FETCH_OK", async () => {
	for (const version of VERSIONS) {
		using fx = serve(version);
		publishPairs(fx.track, 4, 2);
		const ask = async (start: Location, end: Location) =>
			refused(await fetch(fx, new Fetch({ requestId: FETCH_ID, type: range(version, start, end) })));
		const missing = new Fetch({ requestId: FETCH_ID, type: range(version, at(0, 0), at(0, 0), "missing") });
		expect(refused(await fetch(fx, missing))).toBe(DOES_NOT_EXIST);
		// A group below the newest that does not exist, and one past every group.
		expect(await ask(at(2, 0), at(2, 0))).toBe(DOES_NOT_EXIST);
		expect(await ask(at(5, 0), at(5, 0))).toBe(DOES_NOT_EXIST);
		// A start past Largest Object in a group the cache holds (draft-22 section 9.11).
		expect(await ask(at(3, 2), at(3, 6))).toBe(INVALID_RANGE);
		expect(await ask(at(3, 1), at(3, 1))).toBe(INVALID_RANGE);
		// A newest group with no objects starts past Largest Object too.
		writeGroup(fx.track, 4, []);
		expect(await ask(at(4, 0), at(4, 4))).toBe(INVALID_RANGE);
	}
});

// Datagrams are never cached, so a FETCH reaching one finds no group there.
test("a FETCH of a datagram is refused", async () => {
	for (const version of VERSIONS) {
		using fx = serve(version);
		publishPairs(fx.track, 3);
		expect(fx.track.appendDatagram(new Timestamp(0, Timescale.MILLI), new Uint8Array([1]))).toBe(3);
		writeGroup(fx.track, 4, ["4-0"]);

		const only = new Fetch({ requestId: FETCH_ID, type: range(version, at(3, 0), at(3, 0)) });
		expect(refused(await fetch(fx, only))).toBe(DOES_NOT_EXIST);
	}
});

// A filter other than a bounded absolute one resolves against Largest Object.
test("draft-22: a FETCH filter resolves against Largest Object", async () => {
	const cases: [Filter.Filter, number[] | number][] = [
		[{ kind: "unfiltered" }, NOT_SUPPORTED],
		[{ kind: "nextObject" }, INVALID_RANGE],
		[{ kind: "relative", groups: 2n }, NOT_SUPPORTED],
		[{ kind: "relative", groups: 1n }, [2]],
		[{ kind: "absolute", startGroup: 2n, startObject: 0n }, [2]],
	];
	for (const [filter, expected] of cases) {
		using fx = serve(D22);
		publishPairs(fx.track, 3);
		const answer = await fetch(fx, new Fetch({ requestId: FETCH_ID, type: filtered(filter) }));
		if (typeof expected === "number") {
			expect(refused(answer)).toBe(expected);
			continue;
		}
		expect(served(answer).ok.endLocation).toEqual(at(2, 1));
		expect(served(answer).objects).toEqual(pairs(expected));
	}

	// An empty track has no Largest Object to resolve against.
	using fx = serve(D22);
	expect(refused(await fetch(fx, new Fetch({ requestId: FETCH_ID, type: filtered({ kind: "unfiltered" }) })))).toBe(
		NOT_SUPPORTED,
	);
});

test("a FETCH outside the grant is refused", async () => {
	const nothing = (): Grant => ({ publish: new Path.Patterns([]), subscribe: new Path.Patterns([]) });
	for (const version of VERSIONS) {
		using fx = serve(version, new Signal<Grant | undefined>(nothing()));
		publishPairs(fx.track, 2);
		const answer = await fetch(fx, new Fetch({ requestId: FETCH_ID, type: range(version, at(0, 0), at(0, 0)) }));
		expect(refused(answer)).toBe(UNAUTHORIZED);
	}
});

// The requester leaving while the answer waits on an open group is owed nothing, budget or not.
test("a cancelled FETCH stops waiting", async () => {
	for (const version of VERSIONS) {
		for (const fillTimeout of [undefined, Milli(10_000)]) {
			using _clock = manual();
			using fx = serve(version);
			publishPairs(fx.track, 2);
			writeGroup(fx.track, 2, ["2-0"], false);
			const request = requestStream(version);
			const task = fx.pub.runFetch(
				new Fetch({ requestId: FETCH_ID, type: range(version, at(2, 0), at(2, 0)), fillTimeout }),
				request.stream,
			);
			await flush();
			request.withdraw();
			await task;
			const read = await request.answer.done().catch(error);
			expect(read === true || read instanceof Error).toBe(true);
		}
	}
});

// FILL_TIMEOUT is accepted without truncating a one-group answer, including zero.
test("FILL_TIMEOUT is ignored while a group finishes", async () => {
	for (const version of VERSIONS) {
		for (const fillTimeout of [Milli(0), Milli(100)]) {
			using clock = manual();
			using fx = serve(version);
			const live = writeGroup(fx.track, 4, ["4-0"], false);
			let answered = false;
			const answering = fetch(
				fx,
				new Fetch({ requestId: FETCH_ID, type: range(version, at(4, 0), at(4, 0)), fillTimeout }),
			);
			void answering.then(() => {
				answered = true;
			});
			await clock.advance(ms(1000));
			expect(answered).toBe(false);
			live.writeFrame({ payload: new TextEncoder().encode("4-1"), timestamp: new Timestamp(1, Timescale.MILLI) });
			live.finish();
			expect(served(await answering).objects).toEqual(pairs([4]));
		}
	}
});

/** Start a draft-18 Largest Object SUBSCRIBE of `room/video`, or as `fields` override, without reading its answer. */
async function startSubscribe(fx: Served, fields: Partial<ConstructorParameters<typeof Subscribe>[0]> = {}) {
	const client = await Stream.open(fx.pair.client, { version: fx.version });
	const server = await Stream.accept(fx.pair.server, fx.version);
	if (!server) throw new Error("publisher never accepted the subscribe stream");
	const msg = new Subscribe({
		requestId: SUBSCRIBE_ID,
		trackNamespace: Path.from("room"),
		trackName: "video",
		subscriberPriority: 0,
		filter: { kind: "nextObject" },
		...fields,
	});
	return { client, serving: fx.pub.runSubscribe(msg, server) };
}

/** Open a draft-18 SUBSCRIBE of `room/video` with `filter`, returning its SUBSCRIBE_OK. */
async function subscribe(fx: Served, filter: Filter.Filter = { kind: "nextObject" }) {
	const { client, serving } = await startSubscribe(fx, { filter });
	expect(await client.reader.u53()).toBe(SubscribeOk.id);
	const ok = await SubscribeOk.decode(client.reader, fx.version);
	return { client, ok, serving };
}

function joining(offset = 0n): Fetch {
	return new Fetch({ requestId: FETCH_ID, type: { kind: "relativeJoining", subscription: SUBSCRIBE_ID, offset } });
}

// A Largest Object subscription starts right after the Largest Object SUBSCRIBE_OK names,
// and a joining FETCH answers the group's head up to it: together they cover the group with no
// gap or overlap, which is how the Rust subscriber joins mid-group on draft-18.
test("draft-18: a joining FETCH serves the head its subscription starts after", async () => {
	using fx = serve(D18);
	publishPairs(fx.track, 5);
	const live = writeGroup(fx.track, 5, ["5-0", "", "5-2"], false);
	const { client, ok } = await subscribe(fx);
	expect(ok.largest).toEqual({ groupId: 5n, objectId: 2n });

	// Written after SUBSCRIBE_OK, so it belongs to the subscription, not the head.
	live.writeFrame({ payload: new TextEncoder().encode("5-3"), timestamp: new Timestamp(3, Timescale.MILLI) });

	const request = requestStream(D18);
	const task = fx.pub.runFetch(joining(), request.stream);
	// The subscription's stream for group 5 opened first, starting at the object after the head.
	const next = await fx.uni.read();
	if (next.done) throw new Error("no subscription stream");
	const group = new Reader(next.value, undefined, D18);
	const header = await GroupMessage.decode(group, D18);
	expect(header.groupId).toBe(5);
	expect(header.flags.firstObject).toBe(false);
	expect(await group.u53()).toBe(3);

	const head = served(await answer(fx, request.answer, "ascending"));
	await task;
	expect(head.ok).toEqual(
		new FetchOk({
			endOfTrack: false,
			endLocation: at(5, 3),
			properties: { timescale: Timescale.MILLI, priority: 127, groupOrder: 0x2 },
		}),
	);
	expect(head.requestId).toBe(FETCH_ID);
	expect(head.objects).toEqual([
		[5, 0, "5-0"],
		[5, 1, ""],
		[5, 2, "5-2"],
	]);
	// The first object spells its group, object and priority, and the rest inherit them.
	const reader = new Reader(undefined, head.raw, D18);
	await reader.u53();
	await FetchHeader.decode(reader, D18);
	expect(await reader.u53()).toBe(0x3c);
	expect([await reader.u53(), await reader.u53(), await reader.u8()]).toEqual([5, 0, 0]);
	await reader.read(await reader.u53());
	await reader.read(await reader.u53());
	expect(await reader.u53()).toBe(0x20);
	client.close();
});

// A subscription can end while its head is in flight, as when a track finishes right after
// SUBSCRIBE_OK, and the head is then all the subscriber gets of that group.
test("draft-18: a joining FETCH outlives a subscription that ends after FETCH_OK", async () => {
	using fx = serve(D18);
	const live = writeGroup(fx.track, 5, ["5-0", "5-1"], false);
	const { client, serving } = await subscribe(fx);

	const request = requestStream(D18);
	const task = fx.pub.runFetch(joining(), request.stream);
	expect(await request.answer.u53()).toBe(FetchOk.id);
	client.close();
	await serving;
	live.finish();

	// The subscription's own stream for group 5 opened first, and its end reset it.
	const subscription = await fx.uni.read();
	if (subscription.done) throw new Error("no subscription stream");
	const next = await fx.uni.read();
	if (next.done) throw new Error("no fetch stream");
	const reader = new Reader(next.value, undefined, D18);
	expect(await reader.u53()).toBe(FetchHeader.type);
	await FetchHeader.decode(reader, D18);
	expect(await readObjects(reader, "ascending")).toEqual(pairs([5]));
	await task;
});

test("draft-18: a joining FETCH its subscription cannot answer is refused", async () => {
	{
		using fx = serve(D18);
		publishPairs(fx.track, 5);
		// A subscription that never arrives, once the FETCH has waited as long as a request may.
		const clock = new Manual();
		const installed = clock.install();
		try {
			expect(refused(await clock.drive(fetch(fx, joining())))).toBe(INVALID_JOINING_REQUEST_ID);
		} finally {
			installed.close();
		}

		const { client, serving } = await subscribe(fx);
		// Only the subscription's own group, never a run of groups before it.
		expect(refused(await fetch(fx, joining(1n)))).toBe(NOT_SUPPORTED);
		const absolute = new Fetch({
			requestId: FETCH_ID,
			type: { kind: "absoluteJoining", subscription: SUBSCRIBE_ID, group: 5n },
		});
		expect(refused(await fetch(fx, absolute))).toBe(INVALID_RANGE);

		// A FETCH arriving after its subscription ended has no edge to end at.
		client.close();
		await serving;
		const later = new Manual();
		const reinstalled = later.install();
		try {
			expect(refused(await later.drive(fetch(fx, joining())))).toBe(INVALID_JOINING_REQUEST_ID);
		} finally {
			reinstalled.close();
		}
	}
	{
		// A subscription that does not start after Largest Object has no head to join.
		using fx = serve(D18);
		publishPairs(fx.track, 5);
		const { client } = await subscribe(fx, { kind: "unfiltered" });
		expect(refused(await fetch(fx, joining()))).toBe(NOT_SUPPORTED);
		client.close();
	}
	{
		// One that started on an empty track has no head at all.
		using fx = serve(D18);
		const { client } = await subscribe(fx);
		expect(refused(await fetch(fx, joining()))).toBe(INVALID_RANGE);
		client.close();
	}
	{
		// A head the cache lost is refused, rather than answered short of where the
		// subscription starts.
		using fx = serve(D18);
		const live = writeGroup(fx.track, 5, ["5-0", "5-1"], false);
		const { client } = await subscribe(fx);
		live.abort(new Error("evicted"));
		expect(refused(await fetch(fx, joining()))).toBe(DOES_NOT_EXIST);
		client.close();
	}
});

// Request streams race, so a joining FETCH can be read before the SUBSCRIBE it names, as when
// the subscriber pipelines both: it waits for the subscription rather than being refused.
test("draft-18: a joining FETCH waits for the subscription it names", async () => {
	using fx = serve(D18);
	writeGroup(fx.track, 5, ["5-0", "5-1"], false);
	const request = requestStream(D18);
	const task = fx.pub.runFetch(joining(), request.stream);
	await flush();
	const { client } = await subscribe(fx);

	expect(await request.answer.u53()).toBe(FetchOk.id);
	expect((await FetchOk.decode(request.answer, D18)).endLocation).toEqual(at(5, 2));
	// The subscription's own stream for group 5 may open on either side of the fetch stream.
	for (;;) {
		const next = await fx.uni.read();
		if (next.done) throw new Error("no fetch stream");
		const reader = new Reader(next.value, undefined, D18);
		if ((await reader.u53()) !== FetchHeader.type) continue;
		await FetchHeader.decode(reader, D18);
		expect(await readObjects(reader, "ascending")).toEqual(pairs([5]));
		break;
	}
	await task;
	client.close();
});

// A subscription still being set up when the FETCH's request budget runs out is refused TIMEOUT,
// rather than as one the publisher never saw.
test("draft-18: a joining FETCH times out on a subscription still being set up", async () => {
	using fx = serve(D18);
	// Serve tracks on demand and never answer the request, so the SUBSCRIBE never resolves.
	const requested = wireOf(fx.broadcast).requested();
	const { client, serving } = await startSubscribe(fx, { trackName: "audio" });
	expect((await requested)?.name).toBe("audio");

	const clock = new Manual();
	const installed = clock.install();
	try {
		expect(refused(await clock.drive(fetch(fx, joining())))).toBe(TIMEOUT);
	} finally {
		installed.close();
		client.close();
		await serving;
	}
});

// A subscription refused while a joining FETCH waits on it answers the FETCH at once.
test("draft-18: a joining FETCH whose subscription is refused is refused", async () => {
	using fx = serve(D18);
	publishPairs(fx.track, 5);
	const request = requestStream(D18);
	const task = fx.pub.runFetch(joining(), request.stream);
	await flush();

	const { client, serving } = await startSubscribe(fx, { rangeFilters: true });
	expect(await client.reader.u53()).toBe(RequestError.id);
	await serving;

	expect(refused(await answer(fx, request.answer, "ascending"))).toBe(INVALID_JOINING_REQUEST_ID);
	await task;
	client.close();
});
