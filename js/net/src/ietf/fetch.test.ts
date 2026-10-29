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
import { Fetch, FetchHeader, FetchOk, type FetchType, type Location } from "./fetch.ts";
import type * as Filter from "./filter.ts";
import { Group as GroupMessage } from "./object.ts";
import { Publisher } from "./publisher.ts";
import { RequestError } from "./request.ts";
import { NativeSession } from "./session.ts";
import { Subscribe, SubscribeOk } from "./subscribe.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const VERSIONS = [Version.DRAFT_18, Version.DRAFT_22] as const;
const D18 = Version.DRAFT_18;
const D22 = Version.DRAFT_22;
const ALPNS: Record<IetfVersion, string> = { [D18]: ALPN.DRAFT_18, [D22]: ALPN.DRAFT_22 };

const SUBSCRIBE_ID = 7n;
const FETCH_ID = 9n;

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

/** The last Object ID a varint holds, where a gap ends in a group whose end is unknown. */
const MAX_OBJECT = 2n ** 62n - 1n;

/** An End of Range as {@link readObjects} reports it, closing at `group`/`object`. */
function gap(group: number, object: number | bigint, reason: string): [number, number, string] {
	return [group, Number(object), `end ${reason}`];
}

/** What a FILL_TIMEOUT that ran out leaves: Unknown before draft-22 and Timed-Out from it. */
function timedOut(version: IetfVersion): string {
	return version === D18 ? "10c" : "20c";
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

// The groups that do not exist are left out, and the rest go out in the order asked for.
test("a FETCH serves several groups in order", async () => {
	for (const version of VERSIONS) {
		for (const order of ["ascending", "descending"] as const) {
			using fx = serve(version);
			publishPairs(fx.track, 5, 2);
			const type = range(version, at(0, 1), at(3, 1));
			const answer = served(await fetch(fx, new Fetch({ requestId: FETCH_ID, type, groupOrder: order })));
			expect(answer.ok.endLocation).toEqual(endAt(version, 3, 0));
			const groups = [pairs([0]).slice(1), pairs([1]), pairs([3]).slice(0, 1)];
			if (order === "descending") groups.reverse();
			expect(answer.objects).toEqual(groups.flat());
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

		// Capped at the track's last group, so the End Location names its last object.
		const past = served(
			await fetch(fx, new Fetch({ requestId: FETCH_ID, type: range(version, at(3, 0), at(9, 0)) })),
		);
		expect(past.ok.endLocation).toEqual(endAt(version, 4, 1));
		expect(past.ok.endOfTrack).toBe(true);
		expect(past.objects).toEqual(pairs([3, 4]));

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
		const answering = fetch(fx, new Fetch({ requestId: FETCH_ID, type: range(version, at(3, 0), at(9, 0)) }));
		void answering.finally(() => {
			answered = true;
		});
		await flush();
		expect(answered).toBe(false);
		live.writeFrame({ payload: new TextEncoder().encode("4-1"), timestamp: new Timestamp(1, Timescale.MILLI) });
		live.finish();
		const answer = served(await answering);
		expect(answer.ok.endLocation).toEqual(endAt(version, 4, 1));
		expect(answer.ok.endOfTrack).toBe(false);
		expect(answer.objects).toEqual(pairs([3, 4]));
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
test("a FETCH skips a datagram", async () => {
	for (const version of VERSIONS) {
		using fx = serve(version);
		publishPairs(fx.track, 3);
		expect(fx.track.appendDatagram(new Timestamp(0, Timescale.MILLI), new Uint8Array([1]))).toBe(3);
		writeGroup(fx.track, 4, ["4-0"]);

		const answer = served(
			await fetch(fx, new Fetch({ requestId: FETCH_ID, type: range(version, at(1, 0), at(3, 0)) })),
		);
		// The End Location names the last object sent rather than promising the datagram.
		expect(answer.ok.endLocation).toEqual(endAt(version, 2, 1));
		expect(answer.objects).toEqual(pairs([1, 2]));

		const only = new Fetch({ requestId: FETCH_ID, type: range(version, at(3, 0), at(3, 0)) });
		expect(refused(await fetch(fx, only))).toBe(DOES_NOT_EXIST);
	}
});

// A filter other than a bounded absolute one resolves against Largest Object.
test("draft-22: a FETCH filter resolves against Largest Object", async () => {
	const cases: [Filter.Filter, number[] | number][] = [
		[{ kind: "unfiltered" }, [0, 1, 2]],
		[{ kind: "nextObject" }, INVALID_RANGE],
		[{ kind: "relative", groups: 2n }, [1, 2]],
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

// FILL_TIMEOUT=0 asks for only what is cached. The open group's missing tail is a marked gap
// rather than an end the requester would take for the group's, or a wait.
test("a zero FILL_TIMEOUT serves only what is cached", async () => {
	for (const version of VERSIONS) {
		using _clock = manual();
		using fx = serve(version);
		publishPairs(fx.track, 4);
		const live = writeGroup(fx.track, 4, ["4-0"], false);
		const type = range(version, at(3, 0), at(9, 0));
		const answer = served(await fetch(fx, new Fetch({ requestId: FETCH_ID, type, fillTimeout: Milli(0) })));
		// Where the open group ends is unknown, so the gap and the End Location reach the last
		// Object ID there could be: Object 0 covers the whole group before draft-22.
		expect(answer.ok.endLocation).toEqual({ group: 4n, object: version === D18 ? 0n : MAX_OBJECT });
		expect(answer.ok.endOfTrack).toBe(false);
		expect(answer.objects).toEqual([...pairs([3]), [4, 0, "4-0"], gap(4, MAX_OBJECT, timedOut(version))]);

		// A range ending inside the group ends its gap there.
		const bounded = range(version, at(4, 0), at(4, 3));
		const short = served(await fetch(fx, new Fetch({ requestId: FETCH_ID, type: bounded, fillTimeout: Milli(0) })));
		expect(short.ok.endLocation).toEqual(endAt(version, 4, 2));
		expect(short.objects).toEqual([[4, 0, "4-0"], gap(4, 2, timedOut(version))]);
		live.finish();
	}
});

// The budget bounds the total wait: what arrives within it is served, and the rest of the
// group becomes a gap once it runs out, even though the group never finishes.
test("a FILL_TIMEOUT bounds the wait and keeps what arrived within it", async () => {
	for (const version of VERSIONS) {
		using clock = manual();
		using fx = serve(version);
		publishPairs(fx.track, 4);
		const live = writeGroup(fx.track, 4, ["4-0"], false);
		let answered = false;
		const type = range(version, at(3, 0), at(9, 0));
		const answering = fetch(fx, new Fetch({ requestId: FETCH_ID, type, fillTimeout: Milli(100) }));
		void answering.finally(() => {
			answered = true;
		});
		await clock.advance(ms(60));
		live.writeFrame({ payload: new TextEncoder().encode("4-1"), timestamp: new Timestamp(1, Timescale.MILLI) });
		// An object arriving does not restart the budget.
		await clock.advance(ms(39));
		expect(answered).toBe(false);
		await clock.advance(ms(1));
		const answer = served(await answering);
		expect(answer.ok.endOfTrack).toBe(false);
		expect(answer.objects).toEqual([...pairs([3, 4]), gap(4, MAX_OBJECT, timedOut(version))]);
		live.finish();
	}
});

// Groups can arrive out of order. The Largest Object's group is read before FETCH_OK and an
// older open one after it, and both waits draw on the one budget.
test("a FILL_TIMEOUT is one budget across an answer's groups", async () => {
	for (const version of VERSIONS) {
		using clock = manual();
		using fx = serve(version);
		const late = writeGroup(fx.track, 2, ["2-0"], false);
		const newest = writeGroup(fx.track, 3, ["3-0"], false);
		let answered = false;
		const type = range(version, at(2, 0), at(9, 0));
		const answering = fetch(fx, new Fetch({ requestId: FETCH_ID, type, fillTimeout: Milli(100) }));
		void answering.finally(() => {
			answered = true;
		});
		await clock.advance(ms(60));
		newest.writeFrame({ payload: new TextEncoder().encode("3-1"), timestamp: new Timestamp(1, Timescale.MILLI) });
		newest.finish();
		await clock.advance(ms(39));
		expect(answered).toBe(false);
		await clock.advance(ms(1));
		const answer = served(await answering);
		expect(answer.ok.endLocation).toEqual(endAt(version, 3, 1));
		// The object after a gap counts its group from the gap's.
		expect(answer.objects).toEqual([[2, 0, "2-0"], gap(2, MAX_OBJECT, timedOut(version)), ...pairs([3])]);
		late.finish();
	}
});

// A gap covers every Location since the prior object, so a group with nothing to send yet
// first marks the groups skipped before it as what their absence otherwise says: gone.
test("a timed-out group with nothing yet keeps the groups before it nonexistent", async () => {
	for (const version of VERSIONS) {
		for (const order of ["ascending", "descending"] as const) {
			using _clock = manual();
			using fx = serve(version);
			// Group 1 never existed.
			writeGroup(fx.track, 0, ["0-0", "0-1"]);
			const empty = writeGroup(fx.track, 2, [], false);
			writeGroup(fx.track, 3, ["3-0", "3-1"]);
			const type = range(version, at(0, 0), at(3, 0));
			const answer = served(
				await fetch(fx, new Fetch({ requestId: FETCH_ID, type, groupOrder: order, fillTimeout: Milli(0) })),
			);
			expect(answer.ok.endLocation).toEqual(version === D18 ? at(3, 0) : at(3, 1));
			const missing = gap(2, MAX_OBJECT, timedOut(version));
			expect(answer.objects).toEqual(
				order === "ascending"
					? [...pairs([0]), gap(1, MAX_OBJECT, "8c"), missing, ...pairs([3])]
					: [...pairs([3]), gap(3, MAX_OBJECT, "8c"), missing, ...pairs([0])],
			);
			empty.finish();
		}
	}
});

// A group the cache evicts between listing it for the answer and writing it is unknown, not
// skipped, which would say it never existed. Waiting on the open Largest Object's group, before
// FETCH_OK, gives the older groups time to age out of the cache; the newer one stays.
test("a group evicted after it was listed is unknown", async () => {
	for (const version of VERSIONS) {
		for (const order of ["ascending", "descending"] as const) {
			using clock = manual();
			using fx = serve(version);
			// Group 0 never existed; groups 1 and 2 age out.
			writeGroup(fx.track, 1, ["1-0", "1-1"]);
			writeGroup(fx.track, 2, ["2-0", "2-1"]);
			await clock.advance(ms(25_000));
			writeGroup(fx.track, 3, ["3-0", "3-1"]);
			const live = writeGroup(fx.track, 4, ["4-0"], false);

			const type = range(version, at(0, 0), at(4, 0));
			const msg = new Fetch({ requestId: FETCH_ID, type, groupOrder: order, fillTimeout: Milli(20_000) });
			const answer = served(await clock.drive(fetch(fx, msg)));
			expect(answer.ok.endLocation).toEqual({ group: 4n, object: version === D18 ? 0n : MAX_OBJECT });

			const newest: [number, number, string][] = [[4, 0, "4-0"], gap(4, MAX_OBJECT, timedOut(version))];
			expect(answer.objects).toEqual(
				order === "ascending"
					? [
							gap(0, MAX_OBJECT, "8c"),
							gap(1, MAX_OBJECT, "10c"),
							gap(2, MAX_OBJECT, "10c"),
							...pairs([3]),
							...newest,
						]
					: [
							...newest,
							...pairs([3]),
							gap(3, MAX_OBJECT, "8c"),
							gap(2, MAX_OBJECT, "10c"),
							gap(1, MAX_OBJECT, "10c"),
						],
			);
			live.finish();
		}
	}
});

// Every FETCH keeps its own budget, even for the same open group, and one without FILL_TIMEOUT,
// as an older peer sends, waits for the group as it always has.
test("each FETCH keeps its own FILL_TIMEOUT", async () => {
	for (const version of VERSIONS) {
		using clock = manual();
		using fx = serve(version);
		publishPairs(fx.track, 4);
		const live = writeGroup(fx.track, 4, ["4-0"], false);
		const type = range(version, at(4, 0), at(9, 0));
		const waiting = fetch(fx, new Fetch({ requestId: FETCH_ID, type }));
		const cached = served(await fetch(fx, new Fetch({ requestId: FETCH_ID + 1n, type, fillTimeout: Milli(0) })));
		expect(cached.objects).toEqual([[4, 0, "4-0"], gap(4, MAX_OBJECT, timedOut(version))]);

		await clock.advance(ms(10_000));
		live.writeFrame({ payload: new TextEncoder().encode("4-1"), timestamp: new Timestamp(1, Timescale.MILLI) });
		live.finish();
		const full = served(await waiting);
		expect(full.ok.endLocation).toEqual(endAt(version, 4, 1));
		expect(full.objects).toEqual(pairs([4]));
	}
});

/** Open a draft-18 SUBSCRIBE of `room/video` with `filter`, returning its SUBSCRIBE_OK. */
async function subscribe(fx: Served, filter: Filter.Filter = { kind: "nextObject" }) {
	const client = await Stream.open(fx.pair.client, { version: fx.version });
	const server = await Stream.accept(fx.pair.server, fx.version);
	if (!server) throw new Error("publisher never accepted the subscribe stream");
	const msg = new Subscribe({
		requestId: SUBSCRIBE_ID,
		trackNamespace: Path.from("room"),
		trackName: "video",
		subscriberPriority: 0,
		filter,
	});
	const serving = fx.pub.runSubscribe(msg, server);
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

// A subscription refused while a joining FETCH waits on it answers the FETCH at once.
test("draft-18: a joining FETCH whose subscription is refused is refused", async () => {
	using fx = serve(D18);
	publishPairs(fx.track, 5);
	const request = requestStream(D18);
	const task = fx.pub.runFetch(joining(), request.stream);
	await flush();

	const client = await Stream.open(fx.pair.client, { version: D18 });
	const server = await Stream.accept(fx.pair.server, D18);
	if (!server) throw new Error("publisher never accepted the subscribe stream");
	const msg = new Subscribe({
		requestId: SUBSCRIBE_ID,
		trackNamespace: Path.from("room"),
		trackName: "video",
		subscriberPriority: 0,
		filter: { kind: "nextObject" },
		rangeFilters: true,
	});
	const serving = fx.pub.runSubscribe(msg, server);
	expect(await client.reader.u53()).toBe(RequestError.id);
	await serving;

	expect(refused(await answer(fx, request.answer, "ascending"))).toBe(INVALID_JOINING_REQUEST_ID);
	await task;
	client.close();
});
