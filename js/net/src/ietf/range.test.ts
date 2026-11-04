import { expect, onTestFinished, test } from "bun:test";
import { StreamCode, StreamError } from "../error.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream, Writer } from "../stream.ts";
import type { Subscription } from "../track.ts";
import { Fetch, FetchOk } from "./fetch.ts";
import { FetchFrame } from "./object.ts";
import { RequestError } from "./request.ts";
import { NativeSession } from "./session.ts";
import { Subscriber } from "./subscriber.ts";
import { TrackStatusOk, TrackStatusRequest } from "./track.ts";
import { ALPN, type IetfVersion, Version } from "./version.ts";

const VERSIONS = [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22] as const;
const ALPNS = {
	[Version.DRAFT_18]: ALPN.DRAFT_18,
	[Version.DRAFT_20]: ALPN.DRAFT_20,
	[Version.DRAFT_22]: ALPN.DRAFT_22,
};
const ranges = (start: bigint, end: bigint): Subscription => ({
	ranges: [{ start: { absolute: start }, end: { absolute: end } }],
	delay: { max: null },
	order: "ascending",
});

function fixture(version: IetfVersion) {
	const pair = createMockTransportPair(ALPNS[version]);
	const session = new NativeSession(pair.server, version, true);
	const subscriber = new Subscriber({ session });
	const broadcast = subscriber.consume(Path.from("room"));
	const incoming = pair.client.incomingBidirectionalStreams.getReader();
	const readers: { close(): void }[] = [];
	onTestFinished(() => {
		for (const reader of readers) reader.close();
		broadcast.close();
		session.close();
		incoming.releaseLock();
	});
	const next = async () => {
		const stream = await incoming.read();
		if (stream.done) throw new Error("missing request");
		return new Stream({ ...stream.value, version });
	};
	const status = async (largest = 5n, provided?: Stream, typed = false) => {
		const stream = provided ?? (await next());
		if (!typed) expect(await stream.reader.u53()).toBe(TrackStatusRequest.id);
		const request = await TrackStatusRequest.decode(stream.reader, version);
		expect([request.trackNamespace, request.trackName]).toEqual([Path.from("room"), "video"]);
		await stream.writer.u53(TrackStatusOk.id);
		await new TrackStatusOk({ largest: { groupId: largest, objectId: 2n } }).encode(stream.writer, version);
		stream.writer.close();
	};
	const fetch = async (allowStatus = false) => {
		let stream = await next();
		let type = await stream.reader.u53();
		if (type === TrackStatusRequest.id && allowStatus) {
			await status(5n, stream, true);
			stream = await next();
			type = await stream.reader.u53();
		}
		expect(type).toBe(Fetch.id);
		return { stream, request: await Fetch.decode(stream.reader, version) };
	};
	const answer = async (stream: Stream, group: bigint, object = 2n, ended = false) => {
		await stream.writer.u53(FetchOk.id);
		await new FetchOk({
			endOfTrack: ended,
			endLocation: { group, object: version === Version.DRAFT_18 ? object + 1n : object },
		}).encode(stream.writer, version);
	};
	const data = async (id: bigint, groups: bigint[], objects = ["a", "", "c"]) => {
		const bytes = await Writer.collect(version, async (writer) => {
			await writer.u62(id);
			let previousGroup: bigint | undefined;
			for (const group of groups) {
				for (const [object, payload] of objects.entries())
					await new FetchFrame({ payload: new TextEncoder().encode(`${payload && group}${payload}`) }).encode(
						writer,
						{
							group,
							object,
							first: previousGroup === undefined && object === 0,
							previousGroup: object === 0 ? previousGroup : undefined,
						},
						undefined,
						version,
					);
				previousGroup = group;
			}
		});
		return subscriber.handleFetch(new Reader(undefined, bytes, version));
	};
	return { subscriber, broadcast, next, status, fetch, answer, data, readers };
}

async function payloads(group: { readFrame(): Promise<{ payload: Uint8Array } | undefined> }) {
	const out: string[] = [];
	for (let frame = await group.readFrame(); frame; frame = await group.readFrame())
		out.push(new TextDecoder().decode(frame.payload));
	return out;
}

for (const version of VERSIONS) {
	test(`${version}: a one-group FETCH waits for FETCH_OK even when its stream overtakes it`, async () => {
		const fx = fixture(version);
		const pending = fx.broadcast.track("video").fetchGroup(1n);
		await fx.status();
		const { stream, request } = await fx.fetch();
		expect(request.groupOrder).toBe("ascending");
		expect(request.type).toEqual(
			version === Version.DRAFT_18
				? {
						kind: "standalone",
						trackNamespace: Path.from("room"),
						trackName: "video",
						start: { group: 1n, object: 0n },
						end: { group: 1n, object: 0n },
					}
				: {
						kind: "filtered",
						trackNamespace: Path.from("room"),
						trackName: "video",
						filter: {
							kind: "absolute",
							startGroup: 1n,
							startObject: 0n,
							endGroup: 1n,
							endObject: undefined,
						},
					},
		);
		const receiving = fx.data(request.requestId, [1n]);
		await fx.answer(stream, 1n);
		stream.writer.close();
		const group = await pending;
		fx.readers.push(group);
		expect(await payloads(group)).toEqual(["1a", "", "1c"]);
		await receiving;
	});

	test(`${version}: one standalone FETCH answers a past range, including a missing group`, async () => {
		const fx = fixture(version);
		const reader = fx.broadcast.track("video").subscribe(ranges(1n, 5n));
		fx.readers.push(reader);
		const ordered = reader.ordered();
		const read = ordered.nextGroup();
		await fx.status();
		const { stream, request } = await fx.fetch();
		await fx.answer(stream, 4n);
		await fx.data(request.requestId, [1n, 3n, 4n]);
		const first = await read;
		expect(first?.sequence).toBe(1n);
		if (!first) throw new Error("missing first group");
		expect(await payloads(first)).toEqual(["1a", "", "1c"]);
		first.close();
		for (const sequence of [3n, 4n]) {
			const group = await ordered.nextGroup();
			expect(group?.sequence).toBe(sequence);
			if (!group) throw new Error("missing group");
			expect(await payloads(group)).toEqual([`${sequence}a`, "", `${sequence}c`]);
			group.close();
		}
		expect(await ordered.nextGroup()).toBeUndefined();
		const overlapping = fx.broadcast.track("video").subscribe(ranges(0n, 5n));
		fx.readers.push(overlapping);
		const overlap = overlapping.ordered();
		const missing = overlap.nextGroup();
		const backfill = await fx.fetch();
		await fx.answer(backfill.stream, 0n);
		await fx.data(backfill.request.requestId, [0n]);
		const zero = await missing;
		expect(zero?.sequence).toBe(0n);
		zero?.close();
		for (const sequence of [1n, 3n, 4n]) {
			const group = await overlap.nextGroup();
			expect(group?.sequence).toBe(sequence);
			group?.close();
		}
		expect(await overlap.nextGroup()).toBeUndefined();
		overlapping.close();
		reader.update(ranges(5n, 6n));
		const more = ordered.nextGroup();
		const second = await fx.fetch();
		await fx.answer(second.stream, 5n);
		await fx.data(second.request.requestId, [5n]);
		const group = await more;
		expect(group?.sequence).toBe(5n);
		group?.close();
	});

	test(`${version}: a refusal fails its reader and a later reader can ask again`, async () => {
		const fx = fixture(version);
		const pending = fx.broadcast.track("video").fetchGroup(1n);
		const failed = pending.catch((error) => error);
		await fx.status();
		const { stream } = await fx.fetch();
		await stream.writer.u53(RequestError.id);
		await new RequestError({ errorCode: 0x10, reasonPhrase: "missing" }).encode(stream.writer, version);
		expect(await failed).toBeInstanceOf(Error);
		expect((await failed).message).toContain("missing");
		const retry = fx.broadcast.track("video").fetchGroup(1n);
		const second = await fx.fetch(true);
		await fx.answer(second.stream, 1n);
		await fx.data(second.request.requestId, [1n]);
		const group = await retry;
		group.close();
	});

	test(`${version}: a request reset after FETCH_OK refuses the still-missing data stream`, async () => {
		const fx = fixture(version);
		const pending = fx.broadcast.track("video").fetchGroup(1n);
		const failed = pending.catch((error) => error);
		await fx.status();
		const { stream } = await fx.fetch();
		await fx.answer(stream, 1n);
		stream.abort(new StreamError(StreamCode.Cancel));
		expect(await failed).toBeInstanceOf(Error);
	});
	test(`${version}: abandoning a partial group stops its FETCH data stream`, async () => {
		const fx = fixture(version);
		const pending = fx.broadcast.track("video").fetchGroup(1n);
		await fx.status();
		const { stream, request } = await fx.fetch();
		await fx.answer(stream, 1n);
		const prefix = await Writer.collect(version, async (writer) => {
			await writer.u62(request.requestId);
			await new FetchFrame({ payload: new Uint8Array([42]) }).encode(
				writer,
				{ group: 1n, object: 0, first: true },
				undefined,
				version,
			);
		});
		const cancelled = Promise.withResolvers<unknown>();
		const readable = new ReadableStream<Uint8Array>({
			start(controller) {
				controller.enqueue(prefix);
			},
			cancel: cancelled.resolve,
		});
		const receiving = fx.subscriber.handleFetch(new Reader(readable, new Uint8Array(), version));
		const group = await pending;
		expect((await group.readFrame())?.payload).toEqual(new Uint8Array([42]));
		group.close();
		await cancelled.promise;
		await receiving;
	});

	test(`${version}: cancellation before FETCH_OK ends the outstanding request`, async () => {
		const fx = fixture(version);
		const controller = new AbortController();
		const pending = fx.broadcast
			.track("video")
			.fetchGroup(1n, { signal: controller.signal })
			.catch((error) => error);
		await fx.status();
		const { stream } = await fx.fetch();
		controller.abort(new Error("caller left"));
		expect(await pending).toBeInstanceOf(Error);
		await stream.reader.closed.catch(() => undefined);
	});

	test(`${version}: a FETCH object outside the accepted range fails the request`, async () => {
		const fx = fixture(version);
		const pending = fx.broadcast
			.track("video")
			.fetchGroup(1n)
			.catch((error) => error);
		await fx.status();
		const { stream, request } = await fx.fetch();
		await fx.answer(stream, 1n);
		await fx.data(request.requestId, [2n]);
		expect(await pending).toBeInstanceOf(Error);
	});
}
