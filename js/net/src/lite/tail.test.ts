import { describe, expect, test } from "bun:test";
import type { Consumer as GroupConsumer } from "../group.ts";
import { randomHop } from "../hop.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { Milli } from "../time.ts";
import { Group as GroupMessage } from "./group.ts";
import { StreamId } from "./stream.ts";
import {
	encodeSubscribeResponse,
	Subscribe,
	SubscribeDrop,
	SubscribeEnd,
	type SubscribeResponse,
	SubscribeStart,
} from "./subscribe.ts";
import { Subscriber } from "./subscriber.ts";
import { TrackInfo, Track as TrackMessage } from "./track.ts";
import { ALPN_05, Version } from "./version.ts";

// The subscription's max age, which is also how long it waits for a group that never arrives.
const GRACE = Milli(100);

/** One lite-05+ frame: a zero timestamp delta, then the length-prefixed payload. */
function frame(payload: string): Uint8Array {
	const bytes = new TextEncoder().encode(payload);
	// Every field is under 64, so each is a one-byte varint.
	return new Uint8Array([0, bytes.byteLength, ...bytes]);
}

/** A group stream the test writes by hand, handed to the subscriber as if it arrived. */
function groupStream(subscriber: Subscriber, sequence: number) {
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	const readable = new ReadableStream<Uint8Array>({ start: (c) => (controller = c) });
	const handled = subscriber.runGroup(
		new GroupMessage({ subscribe: 0n, sequence }),
		new Reader(readable, undefined, undefined),
	);
	return {
		write: (payload: string) => controller.enqueue(frame(payload)),
		finish: () => controller.close(),
		reset: () => controller.error(new Error("reset")),
		handled,
	};
}

/**
 * A lite-05+ subscriber with one track subscribed, whose publisher the test plays by hand:
 * it answers TRACK_INFO, then writes whatever responses the test asks for on the subscribe
 * stream and FINs it when told.
 */
async function subscribed(version: Version, maxAge = GRACE) {
	const pair = createMockTransportPair(ALPN_05);
	const subscriber = new Subscriber(pair.client, version, randomHop());
	const reader = subscriber.consume(Path.from("room")).track("video").subscribe({ maxAge });

	const info = await Stream.accept(pair.server);
	if (!info) throw new Error("the subscriber never asked for TRACK_INFO");
	expect(await info.reader.u53()).toBe(StreamId.Track);
	await TrackMessage.decode(info.reader, version);
	await new TrackInfo({ maxAge: 60_000 }).encode(info.writer, version);
	info.close();

	const sub = await Stream.accept(pair.server);
	if (!sub) throw new Error("the subscriber never subscribed");
	expect(await sub.reader.u53()).toBe(StreamId.Subscribe);
	await Subscribe.decode(sub.reader, version);

	return {
		subscriber,
		reader,
		respond: (resp: SubscribeResponse) => encodeSubscribeResponse(sub.writer, resp, version),
		fin: () => sub.writer.close(),
	};
}

/** Read one group to its end, or the error it ended with. */
async function readAll(group: GroupConsumer | undefined): Promise<string[]> {
	if (!group) throw new Error("no group");
	const payloads: string[] = [];
	for (;;) {
		const frame = await group.readString();
		if (frame === undefined) return payloads;
		payloads.push(frame);
	}
}

/** Whether `promise` settles within `ms`. */
async function settlesWithin(promise: Promise<unknown>, ms: number): Promise<boolean> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	const pending = new Promise<false>((resolve) => {
		timer = setTimeout(() => resolve(false), ms);
	});
	try {
		return await Promise.race([promise.then(() => true), pending]);
	} finally {
		clearTimeout(timer);
	}
}

describe.each([Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07])("%s", (version) => {
	test("a group stream that arrives after the subscribe stream's FIN is delivered", async () => {
		const { subscriber, reader, respond, fin } = await subscribed(version);
		await respond({ start: new SubscribeStart(0) });
		const first = groupStream(subscriber, 0);
		first.write("0.0");
		first.finish();
		await respond({ end: new SubscribeEnd(2, 2) });
		await fin();

		// The end is known before the last group arrives.
		expect(await reader.finished()).toBe(2);

		// QUIC does not order streams, so group 1 lands after the FIN.
		const late = groupStream(subscriber, 1);
		late.write("1.0");
		late.finish();

		expect(await readAll(await reader.recvGroup())).toEqual(["0.0"]);
		expect(await readAll(await reader.recvGroup())).toEqual(["1.0"]);
		expect(await reader.recvGroup()).toBeUndefined();
		expect(await reader.closed).toBeNull();
		expect(reader.final()).toBe(2);
	});

	test("a group read across the subscribe stream's FIN is delivered whole", async () => {
		const { subscriber, reader, respond, fin } = await subscribed(version);
		await respond({ start: new SubscribeStart(0) });
		const group = groupStream(subscriber, 0);
		group.write("0.0");

		await respond({ end: new SubscribeEnd(1, 1) });
		await fin();
		const received = await reader.recvGroup();
		expect(await received?.readString()).toBe("0.0");

		// The FIN does not end the group: only its own stream does.
		group.write("0.1");
		group.finish();
		expect(await readAll(received)).toEqual(["0.1"]);
		expect(await reader.recvGroup()).toBeUndefined();
		expect(await reader.closed).toBeNull();
	});

	test("a group reset after the subscribe stream's FIN is not presented as complete", async () => {
		const { subscriber, reader, respond, fin } = await subscribed(version);
		await respond({ start: new SubscribeStart(0) });
		const group = groupStream(subscriber, 0);
		group.write("0.0");
		await respond({ end: new SubscribeEnd(1, 1) });
		await fin();

		const received = await reader.recvGroup();
		expect(await received?.readString()).toBe("0.0");
		group.reset();
		await expect(received?.readString() ?? Promise.resolve()).rejects.toThrow();

		// The track still ends cleanly: the group was accounted for, as a reset.
		expect(await reader.recvGroup()).toBeUndefined();
		expect(await reader.closed).toBeNull();
	});

	test("a group that never arrives is given up on after the subscription's max age", async () => {
		const { subscriber, reader, respond, fin } = await subscribed(version);
		await respond({ start: new SubscribeStart(0) });
		const group = groupStream(subscriber, 1);
		group.write("1.0");
		group.finish();
		await respond({ end: new SubscribeEnd(2, 2) });
		await fin();

		// Group 0 was reset before its header arrived, so nothing ever accounts for it.
		expect((await reader.recvGroup())?.sequence).toBe(1);
		const started = performance.now();
		expect(await reader.recvGroup()).toBeUndefined();
		expect(performance.now() - started).toBeGreaterThanOrEqual(GRACE - 5);
		expect(await reader.closed).toBeNull();
		expect(reader.final()).toBe(2);
	});

	test.skipIf(version === Version.DRAFT_07)(
		"a subscription ends without waiting once every group is accounted for",
		async () => {
			// A max age far past the test's patience: only the accounting may end it.
			const { subscriber, reader, respond, fin } = await subscribed(version, Milli(60_000));
			await respond({ start: new SubscribeStart(0) });
			await respond({ drop: new SubscribeDrop({ start: 0, end: 0, error: 0 }) });
			const group = groupStream(subscriber, 1);
			group.write("1.0");
			group.finish();
			await respond({ end: new SubscribeEnd(2, 2) });
			await fin();

			expect((await reader.recvGroup())?.sequence).toBe(1);
			expect(await settlesWithin(reader.recvGroup(), 1000)).toBe(true);
			expect(await reader.closed).toBeNull();
		},
	);

	test("a subscription that served nothing ends at SUBSCRIBE_END", async () => {
		const { reader, respond, fin } = await subscribed(version, Milli(60_000));
		await respond({ end: new SubscribeEnd(0) });
		await fin();

		expect(await settlesWithin(reader.recvGroup(), 1000)).toBe(true);
		expect(await reader.closed).toBeNull();
		expect(reader.final()).toBe(0);
	});

	test("a SUBSCRIBE_END below a group already received aborts the track", async () => {
		const { subscriber, reader, respond } = await subscribed(version);
		await respond({ start: new SubscribeStart(0) });
		const group = groupStream(subscriber, 3);
		group.finish();
		await group.handled;
		await respond({ end: new SubscribeEnd(2, 2) });

		const closed = await reader.closed;
		expect(closed).toBeInstanceOf(Error);
	});
});

test("lite-07 ends without grace when every counted stream arrived despite a skipped group", async () => {
	const { subscriber, reader, respond, fin } = await subscribed(Version.DRAFT_07, Milli(60_000));
	await respond({ start: new SubscribeStart(0) });
	for (const sequence of [0, 2]) {
		const group = groupStream(subscriber, sequence);
		group.write(`${sequence}.0`);
		group.finish();
		await group.handled;
	}
	await respond({ end: new SubscribeEnd(3, 2) });
	await fin();

	expect((await reader.recvGroup())?.sequence).toBe(0);
	expect((await reader.recvGroup())?.sequence).toBe(2);
	expect(await settlesWithin(reader.recvGroup(), 1000)).toBe(true);
	expect(await reader.closed).toBeNull();
});

test("lite-07 zero count ends without grace even when the announced range is nonempty", async () => {
	const { reader, respond, fin } = await subscribed(Version.DRAFT_07, Milli(60_000));
	await respond({ start: new SubscribeStart(0) });
	await respond({ end: new SubscribeEnd(3, 0) });
	await fin();

	expect(await settlesWithin(reader.recvGroup(), 1000)).toBe(true);
	expect(await reader.closed).toBeNull();
	expect(reader.final()).toBe(3);
});
