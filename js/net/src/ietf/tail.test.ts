import { expect, test } from "bun:test";
import { ProtocolViolation } from "../error.ts";
import type { Consumer as GroupConsumer } from "../group.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { TAIL_GRACE_MS } from "../tail.ts";
import { Milli } from "../time.ts";
import { NativeSession } from "./adapter.ts";
import { type GroupFlags, Group as GroupMessage } from "./object.ts";
import { PublishDone } from "./publish.ts";
import { Subscribe, SubscribeOk } from "./subscribe.ts";
import { Subscriber } from "./subscriber.ts";
import { ALPN, Version } from "./version.ts";

const VERSION = Version.DRAFT_19;
const ALIAS = 9n;
const TRACK_ENDED = 0x2;
const INTERNAL_ERROR = 0x0;

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
function groupStream(subscriber: Subscriber, groupId: number) {
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	const readable = new ReadableStream<Uint8Array>({ start: (c) => (controller = c) });
	const header = new GroupMessage({ trackAlias: ALIAS, groupId, subGroupId: 0, publisherPriority: 0, flags: FLAGS });
	const handled = subscriber.handleGroup(header, new Reader(readable, undefined, VERSION));
	return {
		write: (bytes: Uint8Array) => controller.enqueue(bytes),
		finish: () => controller.close(),
		handled,
	};
}

/** A subscriber with one track subscribed and answered; the test plays the publisher. */
async function subscribed() {
	const pair = createMockTransportPair(ALPN.DRAFT_19);
	const session = new NativeSession(pair.server, VERSION, true);
	const subscriber = new Subscriber({ session });
	const reader = subscriber
		.consume(Path.from("room"))
		.track("video")
		.subscribe({ maxAge: Milli(60_000) });

	const peer = await Stream.accept(pair.client, VERSION);
	if (!peer) throw new Error("the subscriber never opened a subscribe stream");
	expect(await peer.reader.u53()).toBe(Subscribe.id);
	const request = await Subscribe.decode(peer.reader, VERSION);
	await peer.writer.u53(SubscribeOk.id);
	await new SubscribeOk({ requestId: request.requestId, trackAlias: ALIAS }).encode(peer.writer, VERSION);

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
	const started = performance.now();
	const late = groupStream(subscriber, 1);
	late.write(object("1.0"));
	late.finish();

	expect(await readAll(await reader.recvGroup())).toEqual(["0.0"]);
	expect(await readAll(await reader.recvGroup())).toEqual(["1.0"]);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(await reader.closed).toBeNull();
	// The Stream Count was met, so nothing waited out the grace.
	expect(performance.now() - started).toBeLessThan(TAIL_GRACE_MS);
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
	const { subscriber, reader, done } = await subscribed();
	const started = performance.now();
	await done(TRACK_ENDED, 0n);

	const late = groupStream(subscriber, 0);
	late.write(object("0.0"));
	late.finish();

	expect(await readAll(await reader.recvGroup())).toEqual(["0.0"]);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(await reader.closed).toBeNull();
	expect(performance.now() - started).toBeGreaterThanOrEqual(TAIL_GRACE_MS - 5);
});

test("a PUBLISH_DONE with an error status aborts the track", async () => {
	const { reader, done } = await subscribed();
	await done(INTERNAL_ERROR, 0n);
	expect(await reader.closed).toBeInstanceOf(Error);
});

test("END_OF_TRACK after a group's last object ends the track after that group", async () => {
	const { subscriber, reader, done } = await subscribed();
	const group = groupStream(subscriber, 4);
	group.write(object("4.0"));
	group.write(END_OF_TRACK);
	group.finish();

	expect(await reader.finished()).toBe(5);
	expect(await readAll(await reader.recvGroup())).toEqual(["4.0"]);

	await done(TRACK_ENDED, 1n);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(reader.final()).toBe(5);
});

test("END_OF_TRACK at object 0 ends the track before its group, which never exists", async () => {
	const { subscriber, reader, done } = await subscribed();
	const group = groupStream(subscriber, 0);
	group.write(object("0.0"));
	group.finish();
	const end = groupStream(subscriber, 2);
	end.write(END_OF_TRACK);
	end.finish();

	expect(await reader.finished()).toBe(2);
	await done(TRACK_ENDED, 2n);
	expect((await reader.recvGroup())?.sequence).toBe(0);
	expect(await reader.recvGroup()).toBeUndefined();
	expect(reader.final()).toBe(2);
});

test("a bare FIN without PUBLISH_DONE aborts the track", async () => {
	const { reader, fin } = await subscribed();
	await fin();
	expect(await reader.closed).toBeInstanceOf(ProtocolViolation);
	await expect(reader.recvGroup()).rejects.toThrow(ProtocolViolation);
});
