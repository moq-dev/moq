import { expect, test } from "bun:test";
import { Instant, Manual, Milli, Nano, now } from "@moq/clock";
import { ProtocolViolation, StreamCode, StreamError } from "../error.ts";
import type { Consumer as GroupConsumer } from "../group.ts";
import { hooks } from "../internal.ts";
import { GRACE } from "../ledger.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { type GroupFlags, Group as GroupMessage } from "./object.ts";
import { PublishDone } from "./publish.ts";
import { NativeSession } from "./session.ts";
import { Subscribe, SubscribeOk } from "./subscribe.ts";
import { Subscriber } from "./subscriber.ts";
import { ALPN, Version } from "./version.ts";

const VERSION = Version.DRAFT_18;
const ALIAS = 9n;
const TRACK_ENDED = 0x2;
const INTERNAL_ERROR = 0x0;
const UNAUTHORIZED = 0x1;
const SUBSCRIPTION_ENDED = 0x3;

// A plain subgroup stream: no extensions, no subgroup id, end of group on FIN.
const FLAGS: GroupFlags = {
	hasExtensions: false,
	hasSubgroup: false,
	hasSubgroupObject: false,
	hasEnd: true,
	hasPriority: true,
	firstObject: true,
};

/** One object with a zero id delta. Every field is under 64, so each is a one-byte varint. */
function object(payload: string): Uint8Array {
	const bytes = new TextEncoder().encode(payload);
	return new Uint8Array([0, bytes.byteLength, ...bytes]);
}

/** An END_OF_TRACK object: zero length, then status 0x4. */
const END_OF_TRACK = new Uint8Array([0, 0, 0x4]);

/** A group stream the test writes by hand, handed to the subscriber as if it arrived. */
function groupStream(subscriber: Subscriber, groupId: number, flags = FLAGS) {
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	const readable = new ReadableStream<Uint8Array>({ start: (c) => (controller = c) });
	const header = new GroupMessage({ trackAlias: ALIAS, groupId, subGroupId: 0, publisherPriority: 0, flags });
	const handled = subscriber.handleGroup(header, new Reader(readable, undefined, VERSION));
	return {
		write: (bytes: Uint8Array) => controller.enqueue(bytes),
		finish: () => controller.close(),
		handled,
	};
}

/** A subscriber with one track subscribed and answered; the test plays the publisher. */
async function subscribed(largest?: number) {
	const pair = createMockTransportPair(ALPN.DRAFT_18);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });
	const reader = subscriber
		.consume(Path.from("room"))
		.track("video")
		.subscribe({ maxDelay: Milli(60_000) });

	const peer = await Stream.accept(pair.client, VERSION);
	if (!peer) throw new Error("the subscriber never opened a subscribe stream");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	await Subscribe.decode(peer.reader, VERSION);
	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({
		trackAlias: ALIAS,
		largest: largest === undefined ? undefined : { groupId: BigInt(largest), objectId: 0n },
	}).encode(peer.writer, VERSION);

	return {
		subscriber,
		reader,
		fin: () => peer.writer.close(),
		done: async (statusCode: number, streamCount: bigint) => {
			await peer.writer.u53(PublishDone.id);
			await new PublishDone({ statusCode, streamCount, reasonPhrase: "done" }).encode(peer.writer, VERSION);
			peer.writer.close();
		},
	};
}

async function readAll(group: GroupConsumer | undefined): Promise<string[]> {
	if (!group) throw new Error("no group");
	const out: string[] = [];
	for (;;) {
		const next = await group.readString();
		if (next === undefined) return out;
		out.push(next);
	}
}

test("a group stream that arrives after PUBLISH_DONE is delivered", async () => {
	const { subscriber, reader, done } = await subscribed();
	const first = groupStream(subscriber, 0);
	first.write(object("0.0"));
	first.finish();
	await first.handled;
	await done(TRACK_ENDED, 2n);

	// QUIC does not order streams, so the second one lands after PUBLISH_DONE.
	const started = now();
	const late = groupStream(subscriber, 1);
	late.write(object("1.0"));
	late.finish();

	expect(await readAll(await reader.recvGroup())).toEqual(["0.0"]);
	expect(await readAll(await reader.recvGroup())).toEqual(["1.0"]);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(await reader.closed).toBeNull();
	// The Stream Count was met, so nothing waited out the grace.
	expect(Nano.toMilli(Instant.durationSince(now(), started))).toBeLessThan(GRACE);
});

test("a group read across PUBLISH_DONE is delivered whole", async () => {
	const { subscriber, reader, done } = await subscribed();
	const group = groupStream(subscriber, 0);
	group.write(object("0.0"));
	await done(TRACK_ENDED, 1n);

	const received = await reader.recvGroup();
	expect(await received?.readString()).toBe("0.0");
	group.write(object("0.1"));
	group.finish();
	expect(await readAll(received)).toEqual(["0.1"]);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(await reader.closed).toBeNull();
});

test("a Stream Count of 0 is a hint, so a late stream within the grace is still delivered", async () => {
	const clock = new Manual();
	const installed = clock.install();
	using _clock = { [Symbol.dispose]: () => installed.close() };
	const { subscriber, reader, done } = await subscribed();
	await done(TRACK_ENDED, 0n);

	const late = groupStream(subscriber, 0);
	late.write(object("0.0"));
	late.finish();

	expect(await readAll(await reader.recvGroup())).toEqual(["0.0"]);

	// The track stays open for another late stream until the grace runs out.
	let ended = false;
	const next = reader.recvGroup().finally(() => {
		ended = true;
	});
	await clock.advance(Nano.fromMilli(Milli(GRACE - 1)));
	expect(ended).toBe(false);
	await clock.advance(Nano.fromMilli(Milli(1)));
	expect(await next).toBeUndefined();
	expect(await reader.closed).toBeNull();
});

test("a PUBLISH_DONE with an error status aborts the track", async () => {
	const { reader, done } = await subscribed();
	await done(INTERNAL_ERROR, 0n);
	expect(await reader.closed).toBeInstanceOf(Error);
});

// SUBSCRIPTION_ENDED ends only the subscription: a later one continues the track past these
// groups, so the copy delivers what was in flight and then fails rather than declare an end.
test("a SUBSCRIPTION_ENDED PUBLISH_DONE drains, then aborts the track", async () => {
	const { subscriber, reader, done } = await subscribed();
	const group = groupStream(subscriber, 0);
	group.write(object("0.0"));
	group.finish();
	await group.handled;
	await done(SUBSCRIPTION_ENDED, 1n);

	expect(await readAll(await reader.recvGroup())).toEqual(["0.0"]);
	expect(await reader.closed).toBeInstanceOf(Error);
	// No end was declared: the track fails rather than finish.
	await expect(reader.finished()).rejects.toBeInstanceOf(Error);
});

test("an UNAUTHORIZED PUBLISH_DONE reads as the revocation a moq-lite peer sends", async () => {
	const { reader, done } = await subscribed();
	await done(UNAUTHORIZED, 0n);
	const closed = await reader.closed;
	expect(closed).toBeInstanceOf(StreamError);
	expect((closed as StreamError).code).toBe(StreamCode.Unauthorized);
});

test("END_OF_TRACK after a group's last object ends the track after that group", async () => {
	const { subscriber, reader, done } = await subscribed();
	const group = groupStream(subscriber, 4);
	group.write(object("4.0"));
	group.write(END_OF_TRACK);
	group.finish();

	expect(await reader.finished()).toEqual({ group: 4, frame: 1 });
	expect(await readAll(await reader.recvGroup())).toEqual(["4.0"]);

	await done(TRACK_ENDED, 1n);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(hooks.subscriberEnd(reader)).toEqual({ group: 4, frame: 1 });
});

test("END_OF_TRACK at object 0 ends the track before its group, which never exists", async () => {
	const { subscriber, reader, done } = await subscribed();
	const group = groupStream(subscriber, 0);
	group.write(object("0.0"));
	group.finish();
	const end = groupStream(subscriber, 2);
	end.write(END_OF_TRACK);
	end.finish();

	expect(await reader.finished()).toEqual({ group: 2, frame: 0 });
	await done(TRACK_ENDED, 2n);
	expect((await reader.recvGroup())?.sequence).toBe(0);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(hooks.subscriberEnd(reader)).toEqual({ group: 2, frame: 0 });
});

test("a standalone END_OF_TRACK at object 5 preserves its terminal group", async () => {
	const { subscriber, reader, done } = await subscribed();
	const end = groupStream(subscriber, 5);
	end.write(new Uint8Array([5, 0, 0x4]));
	end.finish();
	await end.handled;
	expect(hooks.subscriberEnd(reader)).toEqual({ group: 5, frame: 5 });
	await done(TRACK_ENDED, 1n);
	expect(await reader.recvGroup()).toBeUndefined();
});

test("a compact stream starting at object 5 is subgroup 5, refused without a group or a track end", async () => {
	const { subscriber, reader } = await subscribed();
	const payload = groupStream(subscriber, 4, { ...FLAGS, hasSubgroupObject: true });
	payload.write(new Uint8Array([5, 1, 42]));
	payload.finish();
	await payload.handled;
	const end = groupStream(subscriber, 5, { ...FLAGS, hasSubgroupObject: true });
	end.write(new Uint8Array([5, 0, 0x4]));
	end.finish();
	await end.handled;
	// The largest Object ID, past what a number holds exactly, is refused the same way.
	const huge = groupStream(subscriber, 3, { ...FLAGS, hasSubgroupObject: true });
	huge.write(new Uint8Array([...new Array(9).fill(0xff), 1, 42]));
	huge.finish();
	await huge.handled;
	expect(hooks.subscriberEnd(reader)).toBeUndefined();

	const group = groupStream(subscriber, 6, { ...FLAGS, hasSubgroupObject: true });
	group.write(object("6.0"));
	group.finish();
	expect((await reader.recvGroup())?.sequence).toBe(6);
});

// A live join starts at the publisher's Largest, and QUIC does not order streams, so the
// groups from there up to END_OF_TRACK are owed even when a higher one lands first.
test("a reader holds for groups from SUBSCRIBE_OK's Largest that land after a higher one", async () => {
	const { subscriber, reader, done } = await subscribed(3);
	const high = groupStream(subscriber, 5);
	high.write(object("5.0"));
	high.finish();
	await high.handled;
	const end = groupStream(subscriber, 6);
	end.write(END_OF_TRACK);
	end.finish();
	expect(await reader.finished()).toEqual({ group: 6, frame: 0 });
	expect(await readAll(await reader.recvGroup())).toEqual(["5.0"]);

	// Asked before the lower groups land: a reader that ended early returns nothing.
	const held = reader.recvGroup();
	for (const sequence of [3, 4]) {
		const low = groupStream(subscriber, sequence);
		low.write(object(`${sequence}.0`));
		low.finish();
	}
	expect(await readAll(await held)).toEqual(["3.0"]);
	expect(await readAll(await reader.recvGroup())).toEqual(["4.0"]);
	expect(await reader.recvGroup()).toBeUndefined();
	await done(TRACK_ENDED, 4n);
	expect(await reader.closed).toBeNull();
});

test("a group at or past the END_OF_TRACK aborts the track", async () => {
	const { subscriber, reader } = await subscribed();
	const end = groupStream(subscriber, 2);
	end.write(END_OF_TRACK);
	end.finish();
	expect(await reader.finished()).toEqual({ group: 2, frame: 0 });

	const late = groupStream(subscriber, 2);
	late.write(object("2.0"));
	late.finish();
	await late.handled;
	expect(await reader.closed).toBeInstanceOf(ProtocolViolation);
});

test("a bare FIN without PUBLISH_DONE aborts the track", async () => {
	const { reader, fin } = await subscribed();
	await fin();
	expect(await reader.closed).toBeInstanceOf(ProtocolViolation);
	await expect(reader.recvGroup()).rejects.toThrow(ProtocolViolation);
});
