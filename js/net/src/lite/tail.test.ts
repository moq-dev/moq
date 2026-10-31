import { describe, expect, test } from "bun:test";
import { Milli } from "@moq/clock";
import { ProtocolViolation, StreamCode, StreamError } from "../error.ts";
import type { Consumer as GroupConsumer } from "../group.ts";
import { randomHop } from "../hop.ts";
import { hooks } from "../internal.ts";
import { createMockTransportPair } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { Timescale } from "../time.ts";
import type { Groups } from "../track.ts";
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
import { ALPN_05, resolvesStart, Version } from "./version.ts";

// The subscription's max delay, which is also how long it waits for a group that never arrives.
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
		new Reader(readable, undefined, subscriber.version),
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
async function subscribed(version: Version, maxDelay = GRACE, groups?: Groups) {
	const pair = createMockTransportPair(ALPN_05);
	const subscriber = new Subscriber(pair.client, version, randomHop());
	const reader = subscriber.consume(Path.from("room")).track("video").subscribe({ maxDelay, groups });

	const info = await Stream.accept(pair.server, version);
	if (!info) throw new Error("the subscriber never asked for TRACK_INFO");
	expect(await info.reader.u53()).toBe(StreamId.Track);
	await TrackMessage.decode(info.reader, version);
	await new TrackInfo({ maxAge: 60_000, timescale: Timescale.MILLI }).encode(info.writer, version);
	info.close();

	const sub = await Stream.accept(pair.server, version);
	if (!sub) throw new Error("the subscriber never subscribed");
	expect(await sub.reader.u53()).toBe(StreamId.Subscribe);
	await Subscribe.decode(sub.reader, version);

	return {
		subscriber,
		reader,
		respond: (resp: SubscribeResponse) => encodeSubscribeResponse(sub.writer, resp, version),
		fin: () => sub.writer.close(),
		subscriberFin: () => sub.reader.done(),
		reset: (error: Error) => sub.writer.reset(error),
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

/** Let every pending microtask run, so a parked read gets a turn without a timer. */
const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

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
	test("the source start waits for SUBSCRIBE_START where it resolves the start", async () => {
		const { reader, respond } = await subscribed(version, GRACE, { start: { included: 2 } });
		expect(hooks.sourceStart(reader)).toBe(resolvesStart(version) ? "pending" : undefined);
		await respond({ start: new SubscribeStart([3]) });
		await settle();
		expect(hooks.sourceStart(reader)).toBe(3);
		reader.close();
	});

	test("a group stream that arrives after the subscribe stream's FIN is delivered", async () => {
		const { subscriber, reader, respond, fin } = await subscribed(version);
		await respond({ start: new SubscribeStart([0]) });
		const first = groupStream(subscriber, 0);
		first.write("0.0");
		first.finish();
		await respond({ end: new SubscribeEnd(2) });
		await fin();

		// The end is known before the last group arrives.
		expect(await reader.finished()).toEqual({ group: 2, frame: 0 });

		// QUIC does not order streams, so group 1 lands after the FIN.
		const late = groupStream(subscriber, 1);
		late.write("1.0");
		late.finish();

		expect(await readAll(await reader.recvGroup())).toEqual(["0.0"]);
		expect(await readAll(await reader.recvGroup())).toEqual(["1.0"]);
		expect(await reader.recvGroup()).toBeUndefined();
		expect(await reader.closed).toBeNull();
		expect(hooks.subscriberEnd(reader)).toEqual({ group: 2, frame: 0 });
	});

	test("a group read across the subscribe stream's FIN is delivered whole", async () => {
		const { subscriber, reader, respond, fin } = await subscribed(version);
		await respond({ start: new SubscribeStart([0]) });
		const group = groupStream(subscriber, 0);
		group.write("0.0");

		await respond({ end: new SubscribeEnd(1) });
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
		await respond({ start: new SubscribeStart([0]) });
		const group = groupStream(subscriber, 0);
		group.write("0.0");
		await respond({ end: new SubscribeEnd(1) });
		await fin();

		const received = await reader.recvGroup();
		expect(await received?.readString()).toBe("0.0");
		group.reset();
		await expect(received?.readString() ?? Promise.resolve()).rejects.toThrow();

		// The track still ends cleanly: the group was accounted for, as a reset.
		expect(await reader.recvGroup()).toBeUndefined();
		expect(await reader.closed).toBeNull();
	});

	test("readers hold at a missing group until the grace settles the subscription", async () => {
		const realTimeout = globalThis.setTimeout;
		const realNow = performance.now.bind(performance);
		let now = 0;
		performance.now = () => now;
		let expire!: () => void;
		let armed!: () => void;
		const graceArmed = new Promise<void>((resolve) => (armed = resolve));
		globalThis.setTimeout = ((callback: () => void, delay?: number) => {
			if (delay !== GRACE) return realTimeout(callback, delay);
			expire = callback;
			armed();
			// The test advances this clock explicitly, without a wall-clock timer.
			return 0;
		}) as typeof setTimeout;

		let subscriber: Subscriber | undefined;
		try {
			const sub = await subscribed(version);
			subscriber = sub.subscriber;
			const { reader, respond, fin } = sub;
			const ordered = reader.fork({ maxDelay: GRACE }).ordered();
			await respond({ start: new SubscribeStart([0]) });
			const group = groupStream(subscriber, 1);
			group.write("1.0");
			group.finish();
			await group.handled;
			await respond({ end: new SubscribeEnd(2) });
			await fin();
			await graceArmed;

			// Group 0 has no header, so accounting still waits, and its stream may yet
			// land behind group 1's. The arrival reader holds at the hole; the sequence
			// reader is already past it and ends at the boundary.
			expect((await reader.recvGroup())?.sequence).toBe(1);
			let ended = false;
			const held = reader.recvGroup().finally(() => (ended = true));
			expect(await ordered.readString()).toBe("1.0");
			expect(await ordered.readString()).toBeUndefined();
			await settle();
			expect(ended).toBe(false);
			expect(reader.closed.peek()).toBeUndefined();
			expect(hooks.subscriberEnd(reader)).toEqual({ group: 2, frame: 0 });

			// The grace gives up on group 0, and the settled subscription ends the reader.
			now = GRACE;
			expire();
			expect(await held).toBeUndefined();
			expect(await reader.closed).toBeNull();
			ordered.close();
		} finally {
			globalThis.setTimeout = realTimeout;
			performance.now = realNow;
			subscriber?.close();
		}
	});

	// QUIC does not order streams, so a lower group's stream can land after a higher one and
	// after SUBSCRIBE_END. The reader receives it before its end, which then comes at once.
	test("a lower group that lands after SUBSCRIBE_END reaches the reader", async () => {
		const { subscriber, reader, respond, fin } = await subscribed(version, Milli(60_000));
		await respond({ start: new SubscribeStart([0]) });
		const high = groupStream(subscriber, 1);
		high.finish();
		await high.handled;
		await respond({ end: new SubscribeEnd(2) });
		expect(await reader.finished()).toEqual({ group: 2, frame: 0 });

		expect((await reader.recvGroup())?.sequence).toBe(1);
		// Asked before group 0 lands: a reader that ended early returns nothing.
		const held = reader.recvGroup();

		const low = groupStream(subscriber, 0);
		low.write("0.0");
		low.finish();
		await low.handled;
		const late = await held;
		expect(late?.sequence).toBe(0);
		expect(await late?.readString()).toBe("0.0");
		expect(await reader.recvGroup()).toBeUndefined();

		await fin();
		expect(await reader.closed).toBeNull();
		subscriber.close();
	});

	test("a subscription ends without waiting once every group is accounted for", async () => {
		// A max delay far past the test's patience: only the accounting may end it.
		const { subscriber, reader, respond, fin } = await subscribed(version, Milli(60_000));
		await respond({ start: new SubscribeStart([0]) });
		await respond({ drop: new SubscribeDrop({ start: 0, end: 0, error: 0 }) });
		const group = groupStream(subscriber, 1);
		group.write("1.0");
		group.finish();
		await respond({ end: new SubscribeEnd(2) });
		await fin();

		expect((await reader.recvGroup())?.sequence).toBe(1);
		expect(await settlesWithin(reader.recvGroup(), 1000)).toBe(true);
		expect(await reader.closed).toBeNull();
	});

	// Lite-07 carries a stream error code; earlier versions an application one, so the same
	// bytes read as OLD there.
	test("a SUBSCRIBE_DROP ends a group still arriving, before the FIN", async () => {
		const { subscriber, reader, respond } = await subscribed(version, Milli(60_000));
		await respond({ start: new SubscribeStart([0]) });
		const arriving = groupStream(subscriber, 0);
		arriving.write("0.0");
		const group = await reader.recvGroup();
		expect(await group?.readString()).toBe("0.0");

		await respond({ drop: new SubscribeDrop({ start: 0, end: 0, error: 70 }) });
		const err = await group?.readString().then(
			() => undefined,
			(err: unknown) => err,
		);
		expect(err).toBeInstanceOf(StreamError);
		expect((err as StreamError).code).toBe(version === Version.DRAFT_07 ? StreamCode(70) : StreamCode.Old);
		expect(reader.closed.peek()).toBeUndefined();

		// The track goes on: the next group still reaches the reader.
		const following = groupStream(subscriber, 1);
		following.write("1.0");
		const next = await reader.recvGroup();
		expect(next?.sequence).toBe(1);
		expect(await next?.readString()).toBe("1.0");
		subscriber.close();
	});

	test("a subscription that served nothing ends at SUBSCRIBE_END", async () => {
		const { reader, respond, fin } = await subscribed(version, Milli(60_000));
		await respond({ end: new SubscribeEnd(0) });
		await fin();

		expect(await settlesWithin(reader.recvGroup(), 1000)).toBe(true);
		expect(await reader.closed).toBeNull();
		expect(hooks.subscriberEnd(reader)).toEqual({ group: 0, frame: 0 });
	});

	// lite-05 specified an inclusive end, so there the group costs only its own stream.
	test(`a group at or past SUBSCRIBE_END ${version === Version.DRAFT_05 ? "is dropped" : "aborts the track"}`, async () => {
		const { subscriber, reader, respond } = await subscribed(version);
		await respond({ start: new SubscribeStart([0]) });
		await respond({ end: new SubscribeEnd(2) });
		expect(await reader.finished()).toEqual({ group: 2, frame: 0 });

		const handled = groupStream(subscriber, 2).handled;
		if (version === Version.DRAFT_05) {
			await handled;
			expect(await settlesWithin(Promise.resolve(reader.closed), 50)).toBe(false);
		} else {
			// The violation escapes to the connection, which closes the session on it.
			expect(
				await handled.then(
					() => undefined,
					(err: unknown) => err,
				),
			).toBeInstanceOf(ProtocolViolation);
			expect(await reader.closed).toBeInstanceOf(ProtocolViolation);
		}
	});

	test("a floor below SUBSCRIBE_START owes nothing below it", async () => {
		const { subscriber, reader, respond, fin } = await subscribed(version, Milli(60_000), {
			start: { included: 1 },
		});
		await respond({ start: new SubscribeStart([3]) });
		const group = groupStream(subscriber, 3);
		group.finish();
		await group.handled;
		await respond({ end: new SubscribeEnd(4) });
		await fin();

		expect((await reader.recvGroup())?.sequence).toBe(3);
		expect(await settlesWithin(reader.recvGroup(), 1000)).toBe(true);
		expect(await reader.closed).toBeNull();
	});

	test("a lowered floor owes the groups it newly asked for", async () => {
		const maxDelay = Milli(60_000);
		const { subscriber, respond, fin } = await subscribed(version, maxDelay, { start: { included: 3 } });
		await respond({ start: new SubscribeStart([3]) });
		const group = groupStream(subscriber, 3);
		group.write("3.0");
		group.finish();
		await group.handled;

		// A second reader lowers the floor, which the subscription forwards as an update.
		const lower = subscriber
			.consume(Path.from("room"))
			.track("video")
			.subscribe({ maxDelay, groups: { start: { included: 1 } } });
		await respond({ end: new SubscribeEnd(4) });
		await fin();
		// Long enough for a subscription that owed nothing more to have ended.
		await new Promise((resolve) => setTimeout(resolve, 50));

		// QUIC does not order streams, so the lowered groups land after the FIN.
		for (const sequence of [1, 2]) {
			const late = groupStream(subscriber, sequence);
			late.write(`${sequence}.0`);
			late.finish();
			await late.handled;
		}

		const received: number[] = [];
		for (;;) {
			const next = await lower.recvGroup();
			if (!next) break;
			received.push(next.sequence);
		}
		expect(received).toContain(1);
		expect(received).toContain(2);
		expect(await lower.closed).toBeNull();
	});

	// lite-05 specified an inclusive end, and @moq/net 0.1.3 to 0.1.9 sent one, so there an end
	// below a received group only costs the early boundary.
	test(`a SUBSCRIBE_END below a group already received ${version === Version.DRAFT_05 ? "finishes clean" : "aborts the track"}`, async () => {
		const { subscriber, reader, respond, fin } = await subscribed(version);
		await respond({ start: new SubscribeStart([0]) });
		const group = groupStream(subscriber, 3);
		group.finish();
		await group.handled;
		await respond({ end: new SubscribeEnd(2) });

		if (version === Version.DRAFT_05) {
			await fin();
			expect(await reader.closed).toBeNull();
		} else {
			expect(await reader.closed).toBeInstanceOf(ProtocolViolation);
		}
	});
});

test("lite-07 settles a skipped group with SUBSCRIBE_DROP", async () => {
	const { subscriber, reader, respond, fin } = await subscribed(Version.DRAFT_07, Milli(60_000));
	await respond({ start: new SubscribeStart([0]) });
	for (const sequence of [0, 2]) {
		const group = groupStream(subscriber, sequence);
		group.write(`${sequence}.0`);
		group.finish();
		await group.handled;
	}
	await respond({ end: new SubscribeEnd(3) });
	await respond({ drop: new SubscribeDrop({ start: 1, end: 1, error: 0 }) });
	await fin();

	expect((await reader.recvGroup())?.sequence).toBe(0);
	expect((await reader.recvGroup())?.sequence).toBe(2);
	expect(await settlesWithin(reader.recvGroup(), 1000)).toBe(true);
	expect(await reader.closed).toBeNull();
});

test("lite-07 settles a dropped range without grace", async () => {
	const { reader, respond, fin } = await subscribed(Version.DRAFT_07, Milli(60_000));
	await respond({ start: new SubscribeStart([0]) });
	await respond({ end: new SubscribeEnd(3) });
	await respond({ drop: new SubscribeDrop({ start: 0, end: 2, error: 0 }) });
	await fin();

	expect(await settlesWithin(reader.recvGroup(), 1000)).toBe(true);
	expect(await reader.closed).toBeNull();
	expect(hooks.subscriberEnd(reader)).toEqual({ group: 3, frame: 0 });
});

for (const started of [false, true]) {
	test(`a bare FIN ${started ? "after SUBSCRIBE_START" : "without responses"} aborts the track`, async () => {
		const { reader, respond, fin } = await subscribed(Version.DRAFT_05);
		if (started) await respond({ start: new SubscribeStart([0]) });
		await fin();
		expect(await reader.closed).toBeInstanceOf(ProtocolViolation);
		await expect(reader.recvGroup()).rejects.toThrow(ProtocolViolation);
	});
}

test("a subscribe stream reset preserves the publisher's failure", async () => {
	const { reader, reset } = await subscribed(Version.DRAFT_05);
	reset(new StreamError(StreamCode.NotFound));
	const closed = await reader.closed;
	expect(closed).toBeInstanceOf(StreamError);
	expect((closed as StreamError).code).toBe(StreamCode.NotFound);
	await expect(reader.recvGroup()).rejects.toThrow(StreamError);
});

test("lite-07 FIN waits for the skipped and reset final range to settle", async () => {
	const { subscriber, reader, respond, fin, subscriberFin } = await subscribed(Version.DRAFT_07, Milli(60_000));
	await respond({ start: new SubscribeStart([0]) });
	const first = groupStream(subscriber, 0);
	first.finish();
	await first.handled;
	const last = groupStream(subscriber, 2);
	last.write("tail");
	await respond({ end: new SubscribeEnd(3) });
	await respond({ drop: new SubscribeDrop({ start: 1, end: 1, error: 0 }) });
	await fin();
	let finished = false;
	const ack = subscriberFin().then(() => {
		finished = true;
	});
	// Wait for SUBSCRIBE_END to be decoded without advancing any timers.
	while (hooks.subscriberEnd(reader)?.group !== 3) await Promise.resolve();
	expect(finished).toBe(false);
	last.reset();
	await last.handled;
	await ack;
	expect(finished).toBe(true);
	expect(await reader.closed).toBeNull();
});

test("draft-07's SUBSCRIBE_START says the largest before any group arrives", async () => {
	const { reader, respond } = await subscribed(Version.DRAFT_07);
	await respond({ start: new SubscribeStart([4], { group: 4, frame: 2 }) });
	expect(await reader.largest()).toEqual({ group: 4, frame: 2 });
	reader.close();
});
