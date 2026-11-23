import { expect, test } from "bun:test";

import { ObjectDatagram } from "./ietf/datagram.ts";
import * as Filter from "./ietf/filter.ts";
import { GoAway } from "./ietf/goaway.ts";
import * as Message from "./ietf/message.ts";
import { decodeFetchObject, decodeGroupGap, Frame, Group, type GroupFlags } from "./ietf/object.ts";
import { Parameters } from "./ietf/parameters.ts";
import { PublishDone } from "./ietf/publish.ts";
import { RequestError } from "./ietf/request.ts";
import { Version } from "./ietf/version.ts";
import { Fetch } from "./lite/fetch.ts";
import { frameDecoder, Group as LiteGroup } from "./lite/group.ts";
import * as LiteMessage from "./lite/message.ts";
import { Probe } from "./lite/probe.ts";
import { SessionInfo } from "./lite/session.ts";
import { Subscribe, SubscribeStart, SubscribeUpdate, toWireRanges } from "./lite/subscribe.ts";
import { TrackInfo } from "./lite/track.ts";
import { Version as Lite } from "./lite/version.ts";
import * as Path from "./path.ts";
import { Cursor, concat, encodeVarint, Reader, Writer } from "./stream.ts";
import { Timescale } from "./time.ts";
import { type Holdings, rangesOf, union, Window } from "./window.ts";

const UNSAFE = (1n << 53n) + 1n;
const FLAGS: GroupFlags = {
	hasExtensions: false,
	hasSubgroup: true,
	hasSubgroupObject: false,
	hasEnd: true,
	hasPriority: true,
	firstObject: true,
};

function fields(version: Lite | Version, ...values: (number | bigint)[]): Uint8Array {
	return concat(values.map((value) => encodeVarint(value, version)));
}

for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22, Version.DRAFT_23]) {
	for (const [name, decode] of [
		["request error code", RequestError.decode],
		["publish status code", PublishDone.decode],
	] as const) {
		test(`${version}: ${name} refuses an unsafe number on decode`, async () => {
			const bytes = await Writer.collect(version, (w) =>
				Message.encode(w, async (w) => {
					await w.u62(UNSAFE);
					await w.u62(0n);
					await w.string("");
				}),
			);
			await expect(decode(new Reader(undefined, bytes, version), version)).rejects.toThrow(RangeError);
		});
	}
}

// Build the peer's fields independently of each message encoder, so a numeric writer
// cannot reject the fixture before its decoder sees the full-width value.

test("time anchors round-trip beyond 2^53 and resolve adjacent times exactly", async () => {
	const ranges = [{ start: { time: UNSAFE - 1n }, end: { time: UNSAFE } }];
	const message = new Subscribe({ id: UNSAFE, broadcast: Path.from("room"), track: "video", priority: 0, ranges });
	const bytes = await Writer.collect(Lite.DRAFT_07, (w) => message.encode(w, Lite.DRAFT_07));
	expect((await Subscribe.decode(new Reader(undefined, bytes, Lite.DRAFT_07), Lite.DRAFT_07)).ranges).toEqual(ranges);
	const update = new SubscribeUpdate({ priority: 0, ranges });
	const updated = await Writer.collect(Lite.DRAFT_07, (w) => update.encode(w, Lite.DRAFT_07));
	expect((await SubscribeUpdate.decode(new Reader(undefined, updated, Lite.DRAFT_07), Lite.DRAFT_07)).ranges).toEqual(
		ranges,
	);
	// Every group from 0 through the latest held, so a time names the group starting there.
	const contiguous: Holdings = {
		resolves: () => true,
		containing: (time) => ({ at: time }),
		nextStart: (from) => from,
		complete: () => false,
	};
	const model = rangesOf([{ start: UNSAFE - 1n, end: UNSAFE }]);
	expect(new Window(model, UNSAFE + 5n, contiguous).runs()).toEqual([{ start: UNSAFE - 1n, end: UNSAFE + 1n }]);
	expect(union([{ start: UNSAFE - 1n, end: UNSAFE - 1n }, { start: UNSAFE }])).toEqual([{ start: UNSAFE - 1n }]);
	expect(() => rangesOf([{ start: 1 as never }])).toThrow(TypeError);
});

for (const version of [Version.DRAFT_18, Version.DRAFT_20, Version.DRAFT_22, Version.DRAFT_23]) {
	test(`${version}: subgroup and datagram object IDs round-trip beyond 2^53`, async () => {
		await expect(
			Group.decode(new Reader(undefined, fields(version, 2 ** 32 + 0x14, 0, 0, 0, 0), version), version),
		).rejects.toThrow();
		const group = new Group({
			trackAlias: UNSAFE,
			groupId: UNSAFE + 1n,
			subGroupId: UNSAFE + 2n,
			publisherPriority: 0,
			flags: FLAGS,
		});
		const bytes = await Writer.collect(version, (w) => group.encode(w, version));
		expect(await Group.decode(new Reader(undefined, bytes, version), version)).toEqual(group);
		const datagram = new ObjectDatagram({
			trackAlias: UNSAFE,
			groupId: UNSAFE + 1n,
			objectId: UNSAFE + 2n,
			endOfGroup: true,
			body: { payload: new Uint8Array([1]) },
		});
		expect(await ObjectDatagram.decode(datagram.encode(version), version)).toEqual(datagram);
		const fetched = decodeFetchObject(new Cursor(fields(version, 0x1f, UNSAFE, UNSAFE + 1n, 0, 0, 0), version));
		expect(fetched).toMatchObject({ kind: "object", group: UNSAFE, subgroupId: UNSAFE + 1n });
	});

	test(`${version}: numeric clocks refuse unsafe wire milliseconds`, async () => {
		const goaway = await Writer.collect(version, (w) =>
			Message.encode(w, async (w) => {
				await w.string("");
				await w.u62(UNSAFE);
			}),
		);
		await expect(GoAway.decode(new Reader(undefined, goaway, version), version)).rejects.toThrow(RangeError);
		const retry = await Writer.collect(version, (w) =>
			Message.encode(w, async (w) => {
				await w.u53(0);
				await w.u62(UNSAFE);
				await w.string("");
			}),
		);
		await expect(RequestError.decode(new Reader(undefined, retry, version), version)).rejects.toThrow(RangeError);
	});

	// FILL_TIMEOUT is ignored, so the largest varint is as welcome as any other.
	test(`${version}: FILL_TIMEOUT is accepted and ignored at the varint maximum`, async () => {
		const max = (1n << 62n) - 1n;
		await Parameters.decode(new Reader(undefined, fields(version, 1, 0x0a, max), version), version);
		expect(Filter.decodeFill(fields(version, 1, 0x0a, max), version)).toEqual({ rangeFilters: false });
	});

	test(`${version}: object indices, lengths and statuses refuse unsafe numbers`, () => {
		const cursor = (...values: (number | bigint)[]) => new Cursor(fields(version, ...values), version);
		expect(() => Frame.decode(cursor(UNSAFE), FLAGS, true)).toThrow(RangeError);
		expect(() => Frame.decode(cursor(0, UNSAFE), FLAGS)).toThrow(RangeError);
		expect(() => Frame.decode(cursor(0, 0, UNSAFE), FLAGS)).toThrow(RangeError);
		expect(() => decodeFetchObject(cursor(0x0c, 0, UNSAFE))).toThrow(RangeError);
		// An End of Range's Object ID is any varint, since it can close out the rest of a group.
		expect(decodeFetchObject(cursor(0x8c, 0, UNSAFE))).toMatchObject({ kind: "endOfRange", object: UNSAFE });
		expect(() => decodeFetchObject(cursor(0, UNSAFE))).toThrow(RangeError);
		expect(() => decodeGroupGap(cursor(0x0b, UNSAFE))).toThrow(RangeError);
	});
}

for (const version of [Lite.DRAFT_06, Lite.DRAFT_07]) {
	test(`${version}: bounded lite fields refuse unsafe numbers on decode`, async () => {
		const framed = async (body: Uint8Array) =>
			new Reader(
				undefined,
				await Writer.collect(version, (w) => LiteMessage.encode(w, (w) => w.write(body))),
				version,
			);
		// Only lite-06 carries a Frame Start.
		if (version === Lite.DRAFT_06)
			await expect(LiteGroup.decode(await framed(fields(version, 0, 0, UNSAFE)), version)).rejects.toThrow(
				RangeError,
			);
		if (version === Lite.DRAFT_07)
			await expect(
				SubscribeStart.decode(await framed(fields(version, 0, 1, 2, 0, UNSAFE)), version),
			).rejects.toThrow(RangeError);
		for (const index of [0, 1]) {
			const body = await Writer.collect(version, async (w) => {
				await w.string("room");
				if (version === Lite.DRAFT_07) {
					// No pinned instance: empty and unminted, and no node named.
					await w.u53(0);
					await w.bool(false);
					await w.u62(0n);
				}
				await w.string("video");
				await w.u8(0);
				if (version === Lite.DRAFT_07) await w.u53(0);
				await w.u62(0n);
				await w.u62(index === 0 ? UNSAFE : 0n);
				await w.u62(index === 1 ? UNSAFE : 0n);
			});
			await expect(Fetch.decode(await framed(body), version)).rejects.toThrow(RangeError);
		}
		await expect(Probe.decode(await framed(fields(version, UNSAFE)), version)).rejects.toThrow(RangeError);
		await expect(Probe.decode(await framed(fields(version, 0, UNSAFE)), version)).rejects.toThrow(RangeError);
		await expect(TrackInfo.decode(await framed(fields(version, 0, 0, UNSAFE)), version)).rejects.toThrow(
			RangeError,
		);
		const decode = frameDecoder({ scale: Timescale.NANO });
		expect(() => decode(new Cursor(fields(version, UNSAFE << 1n, 0), version))).toThrow(RangeError);
	});
}

test("legacy session bitrate refuses unsafe numbers", async () => {
	const version = Lite.DRAFT_02;
	const bytes = await Writer.collect(version, (w) => LiteMessage.encode(w, (w) => w.u62(UNSAFE)));
	await expect(SessionInfo.decode(new Reader(undefined, bytes, version), version)).rejects.toThrow(RangeError);
});

test("time anchors round-trip beyond 2^53 as group starts", async () => {
	const ranges = toWireRanges([{ start: UNSAFE, end: UNSAFE + 1n }]);
	expect(ranges).toEqual([{ start: { time: UNSAFE }, end: { time: UNSAFE + 1n } }]);
	const message = new Subscribe({ id: 1n, broadcast: Path.from("room"), track: "video", priority: 0, ranges });
	const bytes = await Writer.collect(Lite.DRAFT_07, (w) => message.encode(w, Lite.DRAFT_07));
	expect((await Subscribe.decode(new Reader(undefined, bytes, Lite.DRAFT_07), Lite.DRAFT_07)).ranges).toEqual(ranges);
});
