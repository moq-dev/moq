import { describe, expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { defined } from "../defined.test-util.ts";
import { ProtocolViolation, StreamCode, StreamError, toTransport } from "../error.ts";
import type { Consumer as GroupConsumer } from "../group.ts";
import { randomHop } from "../hop.ts";
import { hooks } from "../internal.ts";
import { createMockTransportPair, manual, settlesWithin } from "../mock.ts";
import * as Path from "../path.ts";
import { Reader, Stream } from "../stream.ts";
import { withMax } from "../subscription.test-util.ts";
import { Timescale } from "../time.ts";
import type { Range } from "../track.ts";
import { Group as GroupMessage } from "./group.ts";
import { StreamId } from "./stream.ts";
import {
	type Edge,
	encodeSubscribeResponse,
	Subscribe,
	SubscribeDrop,
	SubscribeEnd,
	type SubscribeResponse,
	SubscribeStart,
	SubscribeUpdate,
} from "./subscribe.ts";
import { Subscriber } from "./subscriber.ts";
import { TrackInfo, Track as TrackMessage } from "./track.ts";
import { ALPN_05, fillsFromStart, hasLengthFirst, hasRanges, resolvesStart, Version } from "./version.ts";

// The subscription's max delay, which is also how long it waits for a group that never arrives.
const GRACE = Duration.millis(100);

/**
 * One lite-05+ frame: a timestamp delta of `delta` milliseconds, then the length-prefixed
 * payload. Lite-07 leads with the size plus one instead.
 */
function frame(payload: string, version: Version, delta = 0): Uint8Array {
	const bytes = new TextEncoder().encode(payload);
	// Every field is under 64, so each is a one-byte varint; the delta is zigzagged.
	if (delta < 0 || delta >= 32) throw new RangeError("a one-byte delta only");
	if (hasLengthFirst(version)) return new Uint8Array([bytes.byteLength + 1, delta * 2, ...bytes]);
	return new Uint8Array([delta * 2, bytes.byteLength, ...bytes]);
}

/**
 * A group stream the test writes by hand for subscription `subscribe`, handed to the subscriber as
 * if it arrived. Its first frame starts at `sequence` milliseconds, so group starts strictly
 * increase with their keys.
 */
function groupStream(subscriber: Subscriber, key: bigint, subscribe = 0n) {
	let controller!: ReadableStreamDefaultController<Uint8Array>;
	const readable = new ReadableStream<Uint8Array>({ start: (c) => (controller = c) });
	const handled = subscriber.runGroup(
		new GroupMessage({ subscribe, key: key }),
		new Reader(readable, undefined, subscriber.version),
	);
	let first = true;
	return {
		write: (payload: string) => {
			controller.enqueue(frame(payload, subscriber.version, first ? Number(key) : 0));
			first = false;
		},
		finish: () => controller.close(),
		// With a code, as a publisher resets.
		reset: () => controller.error(toTransport(StreamCode.Cancel, "reset")),
		handled,
	};
}

/**
 * A lite-05+ subscriber with one track subscribed, whose publisher the test plays by hand:
 * it answers TRACK_INFO, then writes whatever responses the test asks for on the subscribe
 * stream and FINs it when told.
 */
async function subscribed(version: Version, maxDelay: Duration | null = GRACE, range?: Range) {
	const pair = createMockTransportPair(ALPN_05);
	const subscriber = new Subscriber(pair.client, version, randomHop());
	const reader = subscriber
		.consume(Path.from("room"))
		.track("video")
		.subscribe(withMax({ delay: { max: maxDelay }, ranges: range && [range] }));

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
		update: () => SubscribeUpdate.decode(sub.reader, version),
		fin: () => sub.writer.close(),
		subscriberFin: () => sub.reader.done(),
		reset: (error: Error) => sub.writer.reset(error),
		// The next stream the subscriber opens, such as a recovery's SUBSCRIBE.
		accept: () => Stream.accept(pair.server, version),
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

/**
 * The SUBSCRIBE_START answering range `id` at `start`: on lite-07 with `largest`, by default that
 * the track has no group yet, which a range following the live edge may be answered with.
 */
function answer(version: Version, start: bigint | undefined, id = 0n, largest: Edge = "empty"): SubscribeStart {
	return new SubscribeStart({ id, start, largest: hasRanges(version) ? largest : "unknown" });
}

/** Let every pending microtask run, so a parked read gets a turn without a timer. */
const settle = () => new Promise<void>((resolve) => setImmediate(resolve));

describe.each([Version.DRAFT_05, Version.DRAFT_06, Version.DRAFT_07])("%s", (version) => {
	test("the source start waits for SUBSCRIBE_START where it resolves the start", async () => {
		const { reader, respond } = await subscribed(version, GRACE, { start: 2n });
		// The accepted copy registers once the SUBSCRIBE write resolves, which can trail the publisher
		// reading it, so let that run before asking where the source starts.
		await settle();
		expect(hooks.sourceStart(reader)).toBe(resolvesStart(version) ? "pending" : undefined);
		await respond({ start: answer(version, 3n) });
		await settle();
		expect(hooks.sourceStart(reader)).toBe(3n);
		reader.close();
	});

	test("a group stream that arrives after the subscribe stream's FIN is delivered", async () => {
		const { subscriber, reader, respond, fin } = await subscribed(version);
		await respond({ start: answer(version, 0n) });
		const first = groupStream(subscriber, 0n);
		first.write("0.0");
		first.finish();
		await respond({ end: new SubscribeEnd(2n) });
		await fin();

		// The end is known before the last group arrives.
		expect(await reader.finished()).toEqual({ group: 2n, frame: 0 });

		// QUIC does not order streams, so group 1 lands after the FIN.
		const late = groupStream(subscriber, 1n);
		late.write("1.0");
		late.finish();

		expect(await readAll(await reader.recvGroup())).toEqual(["0.0"]);
		expect(await readAll(await reader.recvGroup())).toEqual(["1.0"]);
		expect(await reader.recvGroup()).toBeUndefined();
		expect(await reader.closed).toBeNull();
		expect(hooks.subscriberEnd(reader)).toEqual({ group: 2n, frame: 0 });
	});

	test("a group read across the subscribe stream's FIN is delivered whole", async () => {
		const { subscriber, reader, respond, fin } = await subscribed(version);
		await respond({ start: answer(version, 0n) });
		const group = groupStream(subscriber, 0n);
		group.write("0.0");

		await respond({ end: new SubscribeEnd(1n) });
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
		await respond({ start: answer(version, 0n) });
		const group = groupStream(subscriber, 0n);
		group.write("0.0");
		await respond({ end: new SubscribeEnd(1n) });
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
		// The test advances this clock explicitly, without a wall-clock timer.
		const clock = new Manual();
		const installed = clock.install();

		let subscriber: Subscriber | undefined;
		try {
			const sub = await subscribed(version);
			subscriber = sub.subscriber;
			const { reader, respond, fin } = sub;
			// History from the oldest group, which owes group 0 until the subscription settles.
			const ordered = reader.fork({ ranges: [{}] }).ordered();
			await respond({ start: answer(version, 0n) });
			const group = groupStream(subscriber, 1n);
			group.write("1.0");
			group.finish();
			await group.handled;
			await respond({ end: new SubscribeEnd(2n) });
			await fin();

			// Group 0 has no header, so accounting still waits, and its stream may yet
			// land behind group 1's. Both readers wait, with the ordered reader holding
			// group 1 until the grace gives up on the owed hole.
			expect((await reader.recvGroup())?.start).toBe(1n);
			let ended = false;
			const held = reader.recvGroup().finally(() => (ended = true));
			const orderedHeld = ordered.readString();
			let orderedReady = false;
			void orderedHeld.then(() => (orderedReady = true));
			await settle();
			expect(orderedReady).toBe(false);
			await settle();
			expect(ended).toBe(false);
			expect(reader.closed.peek()).toBeUndefined();
			expect(hooks.subscriberEnd(reader)).toEqual({ group: 2n, frame: 0 });

			// The grace gives up on group 0, and the settled subscription ends the reader.
			expect(await clock.drive(orderedHeld)).toBe("1.0");
			expect(await clock.drive(held)).toBeUndefined();
			expect(await ordered.readString()).toBeUndefined();
			expect(await reader.closed).toBeNull();
			ordered.close();
		} finally {
			installed.close();
			subscriber?.close();
		}
	});

	// QUIC does not order streams, so a lower group's stream can land after a higher one and
	// after SUBSCRIBE_END. The reader receives it before its end, which then comes at once.
	test("a lower group that lands after SUBSCRIBE_END reaches the reader", async () => {
		// History from the oldest group, which reaches group 0 whenever it lands.
		const { subscriber, reader, respond, fin } = await subscribed(version, null);
		await respond({ start: answer(version, 0n) });
		const high = groupStream(subscriber, 1n);
		high.finish();
		await high.handled;
		await respond({ end: new SubscribeEnd(2n) });
		expect(await reader.finished()).toEqual({ group: 2n, frame: 0 });

		expect((await reader.recvGroup())?.start).toBe(1n);
		// Asked before group 0 lands: a reader that ended early returns nothing.
		const held = reader.recvGroup();

		const low = groupStream(subscriber, 0n);
		low.write("0.0");
		low.finish();
		await low.handled;
		const late = await held;
		expect(late?.start).toBe(0n);
		expect(await late?.readString()).toBe("0.0");
		expect(await reader.recvGroup()).toBeUndefined();

		await fin();
		expect(await reader.closed).toBeNull();
		subscriber.close();
	});

	test("a subscription ends without waiting once every group is accounted for", async () => {
		using clock = manual();
		// A max delay far past the test's patience: only the accounting may end it.
		const { subscriber, reader, respond, fin } = await subscribed(version, Duration.millis(60_000));
		await respond({ start: answer(version, 0n) });
		await respond({ drop: new SubscribeDrop({ start: 0n, end: 1n, error: 0 }) });
		const group = groupStream(subscriber, 1n);
		group.write("1.0");
		group.finish();
		await respond({ end: new SubscribeEnd(2n) });
		await fin();

		expect((await reader.recvGroup())?.start).toBe(1n);
		expect(await settlesWithin(reader.recvGroup(), clock, Duration.millis(1000))).toBe(true);
		expect(await reader.closed).toBeNull();
	});

	// Lite-07 carries a stream error code; earlier versions an application one, so the same
	// bytes read as OLD there.
	test("a SUBSCRIBE_DROP ends a group still arriving, before the FIN", async () => {
		const { subscriber, reader, respond } = await subscribed(version, Duration.millis(60_000));
		await respond({ start: answer(version, 0n) });
		const arriving = groupStream(subscriber, 0n);
		arriving.write("0.0");
		const group = await reader.recvGroup();
		expect(await group?.readString()).toBe("0.0");

		await respond({ drop: new SubscribeDrop({ start: 0n, end: 1n, error: 70 }) });
		const err = await group?.readString().then(
			() => undefined,
			(err: unknown) => err,
		);
		expect(err).toBeInstanceOf(StreamError);
		expect((err as StreamError).code).toBe(version === Version.DRAFT_07 ? StreamCode(70) : StreamCode.Old);
		expect(reader.closed.peek()).toBeUndefined();

		// The track goes on: the next group still reaches the reader.
		const following = groupStream(subscriber, 1n);
		following.write("1.0");
		const next = await reader.recvGroup();
		expect(next?.start).toBe(1n);
		expect(await next?.readString()).toBe("1.0");
		subscriber.close();
	});

	test("a subscription that served nothing ends at SUBSCRIBE_END", async () => {
		using clock = manual();
		const { reader, respond, fin } = await subscribed(version, Duration.millis(60_000));
		await respond({ end: new SubscribeEnd(0n) });
		await fin();

		expect(await settlesWithin(reader.recvGroup(), clock, Duration.millis(1000))).toBe(true);
		expect(await reader.closed).toBeNull();
		expect(hooks.subscriberEnd(reader)).toEqual({ group: 0n, frame: 0 });
	});

	// lite-05 specified an inclusive end, so there the group costs only its own stream.
	test(`a group at or past SUBSCRIBE_END ${version === Version.DRAFT_05 ? "is dropped" : "aborts the track"}`, async () => {
		using clock = manual();
		const { subscriber, reader, respond } = await subscribed(version);
		await respond({ start: answer(version, 0n) });
		await respond({ end: new SubscribeEnd(2n) });
		expect(await reader.finished()).toEqual({ group: 2n, frame: 0 });

		const handled = groupStream(subscriber, 2n).handled;
		if (version === Version.DRAFT_05) {
			await handled;
			expect(await settlesWithin(Promise.resolve(reader.closed), clock, Duration.millis(50))).toBe(false);
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
		using clock = manual();
		const { subscriber, reader, respond, fin, accept } = await subscribed(version, Duration.millis(60_000), {
			start: 1n,
		});
		await respond({ start: answer(version, 3n) });
		const group = groupStream(subscriber, 3n);
		group.finish();
		await group.handled;
		expect((await reader.recvGroup())?.start).toBe(3n);
		// The reader still asks for the groups below. Unless its fills stop at the start, the copy asks
		// upstream for them on a SUBSCRIBE of its own, and stays open past the track's end while it is out.
		// Lite-07 asks its range from a time, which the start lands at group 3, so nothing below is owed.
		const ended = reader.recvGroup();
		const recovery = fillsFromStart(version) || hasRanges(version) ? undefined : defined(await accept());
		await respond({ end: new SubscribeEnd(4n) });
		await fin();
		if (recovery) {
			expect(await recovery.reader.u53()).toBe(StreamId.Subscribe);
			await Subscribe.decode(recovery.reader, version);
			// Once the reader's miss is answered otherwise, the copy may cancel it first, which stops
			// these writes: the reader ends either way.
			const answered = async () => {
				await encodeSubscribeResponse(recovery.writer, { start: answer(version, 3n) }, version);
				await encodeSubscribeResponse(recovery.writer, { end: new SubscribeEnd(4n) }, version);
				recovery.writer.close();
			};
			await answered().catch(() => undefined);
		}
		expect(await settlesWithin(ended, clock, Duration.millis(1000))).toBe(true);
		expect(await reader.closed).toBeNull();
	});

	// Lite-07 answers the lowered range with a SUBSCRIBE_START of its own. Before it nothing answers
	// an update, so the publisher may have sent its FIN before reading one: what it newly asked for
	// below SUBSCRIBE_START is not waited for, which would hold the publisher's draining close.
	test(`a lowered floor ${hasRanges(version) ? "owes the groups it newly asked for" : "owes nothing below SUBSCRIBE_START"}`, async () => {
		using clock = manual();
		const maxDelay = Duration.millis(60_000);
		const { subscriber, respond, update, fin, subscriberFin } = await subscribed(version, maxDelay, {
			start: 3n,
		});
		await respond({ start: answer(version, 3n) });
		const group = groupStream(subscriber, 3n);
		group.write("3.0");
		group.finish();
		await group.handled;

		// A second reader lowers the floor, which the subscription forwards as an update.
		const lower = subscriber
			.consume(Path.from("room"))
			.track("video")
			.subscribe(withMax({ delay: { max: maxDelay }, ranges: [{ start: 1n }] }));
		// Lite-07 retires the range and adds the lowered one, whose start SUBSCRIBE_START reports.
		const lowered = await update();
		if (hasRanges(version)) {
			// A group key goes out as the time it starts at.
			expect([lowered.remove, lowered.ranges]).toEqual([[0n], [{ start: { time: 1n } }]]);
			await respond({ start: answer(version, 1n, 1n) });
		}
		await respond({ end: new SubscribeEnd(4n) });
		await fin();
		// Long enough for a subscription that owed nothing more to have ended.
		const finished = await settlesWithin(subscriberFin(), clock, Duration.millis(50));
		expect(finished).toBe(!hasRanges(version));
		if (!hasRanges(version)) return;

		// QUIC does not order streams, so the lowered groups land after the FIN.
		for (const key of [1n, 2n]) {
			const late = groupStream(subscriber, key);
			late.write(`${key}.0`);
			late.finish();
			await late.handled;
		}

		const received: bigint[] = [];
		for (;;) {
			const next = await lower.recvGroup();
			if (!next) break;
			received.push(next.start);
		}
		expect(received).toContain(1n);
		expect(received).toContain(2n);
		expect(await lower.closed).toBeNull();
	});

	// lite-05 specified an inclusive end, and @moq/net 0.1.3 to 0.1.9 sent one, so there an end
	// below a received group only costs the early boundary.
	test(`a SUBSCRIBE_END below a group already received ${version === Version.DRAFT_05 ? "finishes clean" : "aborts the track"}`, async () => {
		using clock = manual();
		const { subscriber, reader, respond, fin } = await subscribed(version);
		await respond({ start: answer(version, 0n) });
		const group = groupStream(subscriber, 3n);
		group.finish();
		await group.handled;
		await respond({ end: new SubscribeEnd(2n) });

		if (version === Version.DRAFT_05) {
			await fin();
			// Groups 0 and 1 never arrive, so the subscription ends once their grace runs out.
			expect(await clock.drive(Promise.resolve(reader.closed))).toBeNull();
		} else {
			expect(await reader.closed).toBeInstanceOf(ProtocolViolation);
		}
	});
});

test("lite-07 settles a skipped group with SUBSCRIBE_DROP", async () => {
	using clock = manual();
	const { subscriber, reader, respond, fin } = await subscribed(Version.DRAFT_07, Duration.millis(60_000));
	await respond({ start: answer(Version.DRAFT_07, 0n) });
	for (const key of [0n, 2n]) {
		const group = groupStream(subscriber, key);
		group.write(`${key}.0`);
		group.finish();
		await group.handled;
	}
	await respond({ end: new SubscribeEnd(3n) });
	await respond({ drop: new SubscribeDrop({ start: 1n, end: 2n, error: 0 }) });
	await fin();

	expect((await reader.recvGroup())?.start).toBe(0n);
	expect((await reader.recvGroup())?.start).toBe(2n);
	expect(await settlesWithin(reader.recvGroup(), clock, Duration.millis(1000))).toBe(true);
	expect(await reader.closed).toBeNull();
});

test("lite-07 settles a dropped range without grace", async () => {
	using clock = manual();
	const { reader, respond, fin } = await subscribed(Version.DRAFT_07, Duration.millis(60_000));
	await respond({ start: answer(Version.DRAFT_07, 0n) });
	await respond({ end: new SubscribeEnd(3n) });
	await respond({ drop: new SubscribeDrop({ start: 0n, end: 3n, error: 0 }) });
	await fin();

	expect(await settlesWithin(reader.recvGroup(), clock, Duration.millis(1000))).toBe(true);
	expect(await reader.closed).toBeNull();
	expect(hooks.subscriberEnd(reader)).toEqual({ group: 3n, frame: 0 });
});

for (const started of [false, true]) {
	test(`a bare FIN ${started ? "after SUBSCRIBE_START" : "without responses"} aborts the track`, async () => {
		const { reader, respond, fin } = await subscribed(Version.DRAFT_05);
		if (started) await respond({ start: answer(Version.DRAFT_05, 0n) });
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
	const { subscriber, reader, respond, fin, subscriberFin } = await subscribed(
		Version.DRAFT_07,
		Duration.millis(60_000),
	);
	await respond({ start: answer(Version.DRAFT_07, 0n) });
	const first = groupStream(subscriber, 0n);
	first.finish();
	await first.handled;
	const last = groupStream(subscriber, 2n);
	last.write("tail");
	await respond({ end: new SubscribeEnd(3n) });
	await respond({ drop: new SubscribeDrop({ start: 1n, end: 2n, error: 0 }) });
	await fin();
	let finished = false;
	const ack = subscriberFin().then(() => {
		finished = true;
	});
	// Wait for SUBSCRIBE_END to be decoded without advancing any timers.
	while (hooks.subscriberEnd(reader)?.group !== 3n) await Promise.resolve();
	expect(finished).toBe(false);
	last.reset();
	await last.handled;
	await ack;
	expect(finished).toBe(true);
	expect(await reader.closed).toBeNull();
});

test("draft-07's SUBSCRIBE_START says the largest before any group arrives", async () => {
	const { reader, respond } = await subscribed(Version.DRAFT_07);
	await respond({ start: answer(Version.DRAFT_07, 4n, 0n, { position: { group: 4n, frame: 2 }, ended: false }) });
	expect(await reader.largest()).toEqual({ group: 4n, frame: 2 });
	reader.close();
});

// TEMPORARY MIGRATION, delete with `RecoveryHandle.walk`: a relay of the previous release starts a
// recovery at its floor, sends the next group it holds, and never accounts for the keys between.
// Once the grace passes with no progress, the copy FETCHes them a key at a time instead. Mirrors the Rust
// `a_stalled_recovery_walks_its_keys`, on lite-05, where a floor below the start is recovered.
test("a recovery stalled behind a relay of the previous release is fetched", async () => {
	using clock = manual();
	const version = Version.DRAFT_05;
	const { subscriber, reader, respond, fin, accept } = await subscribed(version, Duration.millis(60_000), {
		start: 1n,
	});
	await respond({ start: answer(version, 4n) });
	const group = groupStream(subscriber, 4n);
	group.finish();
	await group.handled;
	expect((await reader.recvGroup())?.start).toBe(4n);
	// The reader still owes 1..4, which the copy recovers on a SUBSCRIBE of its own.
	void reader.recvGroup().catch(() => undefined);
	const recovery = defined(await accept());
	expect(await recovery.reader.u53()).toBe(StreamId.Subscribe);
	await Subscribe.decode(recovery.reader, version);
	await encodeSubscribeResponse(recovery.writer, { start: answer(version, 1n) }, version);
	const held = groupStream(subscriber, 3n, 1n);
	held.write("3.0");

	// The grace passes while group 3 is still being read, which is waited on rather than spun on.
	const fetched = accept();
	expect(await settlesWithin(fetched, clock, Duration.millis(2000))).toBe(false);
	// Its finishing is progress too, so the grace starts over from there.
	held.finish();
	await held.handled;
	expect(await settlesWithin(fetched, clock, Duration.millis(900))).toBe(false);
	expect(await settlesWithin(fetched, clock, Duration.millis(200))).toBe(true);
	const fetch = defined(await fetched);
	expect(await fetch.reader.u53()).toBe(StreamId.Fetch);

	// The subscription ends while the walk is still out, which the copy waits for. Upstream answers
	// keys 1 and 2 not found, and then the reader ends cleanly.
	await respond({ end: new SubscribeEnd(5n) });
	await fin();
	const closed = reader.closed;
	expect(await settlesWithin(closed, clock, Duration.millis(100))).toBe(false);
	fetch.abort(new StreamError(StreamCode.NotFound));
	const second = defined(await accept());
	expect(await second.reader.u53()).toBe(StreamId.Fetch);
	second.abort(new StreamError(StreamCode.NotFound));
	expect(await settlesWithin(closed, clock, Duration.millis(100))).toBe(true);
});

// A current relay answers the keys below the first group once its own upstream does, which may
// take longer than the grace in all: each answer restarts the grace, so the copy waits on a relay
// still answering rather than fetching the run a key at a time. Mirrors the Rust
// `a_slow_relay_still_answering_is_not_walked`.
test("a recovery behind a slow relay still answering is not fetched", async () => {
	using clock = manual();
	const version = Version.DRAFT_05;
	const { subscriber, reader, respond, accept } = await subscribed(version, Duration.millis(60_000), {
		start: 1n,
	});
	await respond({ start: answer(version, 4n) });
	const group = groupStream(subscriber, 4n);
	group.finish();
	await group.handled;
	expect((await reader.recvGroup())?.start).toBe(4n);
	void reader.recvGroup().catch(() => undefined);
	const recovery = defined(await accept());
	expect(await recovery.reader.u53()).toBe(StreamId.Subscribe);
	await Subscribe.decode(recovery.reader, version);
	await encodeSubscribeResponse(recovery.writer, { start: answer(version, 1n) }, version);
	const first = groupStream(subscriber, 3n, 1n);
	first.finish();
	await first.handled;

	// Keys 1 and 2 are answered 800 ms apart, 1.6 s in all.
	const fetched = accept();
	expect(await settlesWithin(fetched, clock, Duration.millis(800))).toBe(false);
	await encodeSubscribeResponse(
		recovery.writer,
		{ drop: new SubscribeDrop({ start: 1n, end: 2n, error: 0 }) },
		version,
	);
	expect(await settlesWithin(fetched, clock, Duration.millis(800))).toBe(false);
	await encodeSubscribeResponse(
		recovery.writer,
		{ drop: new SubscribeDrop({ start: 2n, end: 3n, error: 0 }) },
		version,
	);
	expect(await settlesWithin(fetched, clock, Duration.millis(2000))).toBe(false);
});

// A lite-05 publisher that finished the track FINs a recovery before the group streams it opened
// land. Nothing names a key after that FIN, so one that never lands is given up after the grace,
// not the reader's max delay. Mirrors the Rust `a_drained_recovery_gives_up_a_missing_key`.
test("a drained recovery gives up a missing key after the grace", async () => {
	using clock = manual();
	const version = Version.DRAFT_05;
	const { subscriber, reader, respond, accept } = await subscribed(version, Duration.millis(60_000), {
		start: 1n,
	});
	await respond({ start: answer(version, 4n) });
	const group = groupStream(subscriber, 4n);
	group.finish();
	await group.handled;
	expect((await reader.recvGroup())?.start).toBe(4n);
	// The reader still owes 1..4, which the copy recovers on a SUBSCRIBE of its own.
	void reader.recvGroup().catch(() => undefined);
	const recovery = defined(await accept());
	expect(await recovery.reader.u53()).toBe(StreamId.Subscribe);
	await Subscribe.decode(recovery.reader, version);
	await encodeSubscribeResponse(recovery.writer, { start: answer(version, 1n) }, version);
	await encodeSubscribeResponse(recovery.writer, { end: new SubscribeEnd(5n) }, version);
	recovery.writer.close();

	// The copy closes its side once it gives up.
	const ended = recovery.reader.done();
	expect(await settlesWithin(ended, clock, Duration.millis(900))).toBe(false);
	expect(await settlesWithin(ended, clock, Duration.millis(200))).toBe(true);
});
