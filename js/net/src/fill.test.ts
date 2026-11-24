import { expect, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import { DEFAULT_EXPIRY } from "./cache.ts";
import { accept as acceptSession, connect as connectSession } from "./connection/index.ts";
import { defined } from "./defined.test-util.ts";
import { NotFound, StreamCode, StreamError, toStreamCode } from "./error.ts";
import { rejectGroups } from "./fill.ts";
import type { Consumer as GroupConsumer, Producer as GroupProducer } from "./group.ts";
import { hooks, staleHole } from "./internal.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { withMax } from "./subscription.test-util.ts";
import { Timescale, Timestamp } from "./time.ts";
import type { FillGroups, Producer, Requests, Subscription } from "./track.ts";
import { wireOf } from "./wire.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

// Every track here states no publisher max age: a fill serves history, which the default zero
// max age would leave stale.

// The archive an in-memory responder serves: each group's frames, by sequence.
type Archive = Map<bigint, string[]>;

// Serve every fill from `archive` until the track closes, dropping what it lacks. Returns the
// fills it took, so a test can count them.
function respond(requests: Requests, archive: Archive): FillGroups[] {
	const fills: FillGroups[] = [];
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			fills.push(fill);
			serve(fill, archive);
		}
	})();
	return fills;
}

function serve(fill: FillGroups, archive: Archive): void {
	for (let key = fill.wanted(); key !== undefined; key = fill.wanted()) {
		const frames = archive.get(key);
		if (!frames) {
			fill.dropGroups(key, key + 1n);
			continue;
		}
		const group = defined(fill.accept(key));
		for (const frame of frames)
			group.writeFrame({ payload: enc.encode(frame), timestamp: Timestamp.unscaled(group.start) });
		group.finish();
	}
	fill.close();
}

async function frames(group: GroupConsumer): Promise<string[]> {
	const out: string[] = [];
	for (let frame = await group.readFrame(); frame; frame = await group.readFrame())
		out.push(dec.decode(frame.payload));
	return out;
}

// Read `reader` until it ends, closing each group.
async function drain(reader: { recvGroup(): Promise<GroupConsumer | undefined> }): Promise<void> {
	try {
		for (let group = await reader.recvGroup(); group; group = await reader.recvGroup()) group.close();
	} catch {
		// Closed by the test.
	}
}

// A track whose first write is group `first`, so every group below it was never cached here.
function resumed(first: bigint, maxAge: Duration | null = null): { broadcast: BroadcastProducer; track: Producer } {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { maxAge }));
	const group = track.createGroup(first);
	group.writeFrame({ payload: enc.encode(`live ${first}`), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
	return { broadcast, track };
}

const history: Subscription = withMax({ ranges: [{ start: 0n }], delay: { max: null }, order: "ascending" });

test("fill: a fetch of a group never cached is served by the request handler, byte for byte", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const fills = respond(requests, new Map([[2n, ["a", "b", "c"]]]));

	const group = await wireOf(broadcast).fetchGroup("video", 2n);
	expect(group.start).toBe(2n);
	expect(await frames(group)).toEqual(["a", "b", "c"]);
	expect(fills.map((fill) => [fill.start, fill.end])).toEqual([[2n, 3n]]);

	// Now cached: a second fetch asks nothing.
	const again = await wireOf(broadcast).fetchGroup("video", 2n);
	expect(await frames(again)).toEqual(["a", "b", "c"]);
	expect(fills.length).toBe(1);

	requests.close();
	broadcast.close();
});

test("fill: an empty group replays as empty, and a missing one fails the fetch with NotFound", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	respond(requests, new Map([[1n, []]]));

	const empty = await wireOf(broadcast).fetchGroup("video", 1n);
	expect(await frames(empty)).toEqual([]);
	await expect(wireOf(broadcast).fetchGroup("video", 3n)).rejects.toBeInstanceOf(NotFound);

	requests.close();
	broadcast.close();
});

test("fill: without a handler, a fetch of an uncached group fails at once", async () => {
	const { broadcast } = resumed(5n);
	await expect(wireOf(broadcast).fetchGroup("video", 2n)).rejects.toBeInstanceOf(NotFound);
	broadcast.close();
});

test("fill: without a handler, a fetch of a group not written yet fails at once, as in Rust", async () => {
	const { broadcast } = resumed(5n);
	await expect(wireOf(broadcast).fetchGroup("video", 9n)).rejects.toBeInstanceOf(NotFound);
	broadcast.close();
});

test("fill: a fetch of a group about to be written asks the handler at once, and the write answers it", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const fetching = wireOf(broadcast).fetchGroup("video", 7n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect([fill.start, fill.end]).toEqual([7n, 8n]);
	expect(fill.wanted()).toBe(7n);

	// The producer gets there before the handler answers: the live write is the answer.
	const group = track.createGroup(7n);
	group.writeFrame({ payload: enc.encode("written"), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
	expect(fill.wanted()).toBeUndefined();
	expect(await frames(await fetching)).toEqual(["written"]);

	fill.close();
	requests.close();
	broadcast.close();
});

test("fill: a fetch of a group not written yet that the handler lacks fails with NotFound", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const fills = respond(requests, new Map());
	await expect(wireOf(broadcast).fetchGroup("video", 9n)).rejects.toBeInstanceOf(NotFound);
	expect(fills.map((fill) => [fill.start, fill.end])).toEqual([[9n, 10n]]);
	requests.close();
	broadcast.close();
});

test("fill: concurrent fetches of one group share one fill", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const a = wireOf(broadcast).fetchGroup("video", 3n);
	const b = wireOf(broadcast).fetchGroup("video", 3n);

	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect([fill.start, fill.end]).toEqual([3n, 4n]);
	// The second fetch joined it rather than queueing its own.
	await yieldNow();
	const queued = await Promise.race([requests.requestedFill(), yieldNow().then(() => "none")]);
	expect(queued).toBe("none");

	serve(fill, new Map([[3n, ["x"]]]));
	const [first, second] = await Promise.all([a, b]);
	expect(await frames(first)).toEqual(["x"]);
	expect(await frames(second)).toEqual(["x"]);

	requests.close();
	broadcast.close();
});

test("fill: a fetch cancelled while its fill awaits a reply withdraws the fill", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const controller = new AbortController();
	const fetching = wireOf(broadcast).fetchGroup("video", 2n, { signal: controller.signal });

	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect(await fill.wantedChanged()).toBe(2n);
	expect(fill.isWanted(2n)).toBe(true);

	const reason = new Error("left");
	controller.abort(reason);
	await expect(fetching).rejects.toBe(reason);
	expect(await fill.wantedChanged()).toBeUndefined();
	expect(fill.isWanted(2n)).toBe(false);
	// Nothing reads it, so closing the withdrawn fill records nothing.
	fill.close();
	const later = wireOf(broadcast).fetchGroup("video", 2n);
	const refill = await requests.requestedFill();
	expect(refill?.start).toBe(2n);
	if (refill) serve(refill, new Map([[2n, ["late"]]]));
	expect(await frames(await later)).toEqual(["late"]);

	requests.close();
	broadcast.close();
});

// As in Rust: a failed fill answer holds nothing the feed promised, so it never holds the end.
test("fill: an answer aborted below the end does not hold the end open", async () => {
	for (const parked of [false, true]) {
		const { broadcast, track } = resumed(1n);
		const requests = track.requests();
		const fetching = wireOf(broadcast).fetchGroup("video", 0n);
		const fill = await requests.requestedFill();
		if (!fill) throw new Error("no fill of groups");
		const group = defined(fill.accept(0n));
		group.writeFrame({ payload: enc.encode("backfill"), timestamp: Timestamp.unscaled(group.start) });
		if (parked) {
			track.finishAt({ group: 2n, frame: 0 });
			await yieldNow();
			expect(track.closed.peek()).toBeUndefined();
		}
		group.abort(new StreamError(StreamCode.Cancel));
		fill.close();
		await fetching.catch(() => undefined);
		if (!parked) track.finishAt({ group: 2n, frame: 0 });
		await yieldNow();
		expect(track.closed.peek()).toBeNull();
		track.abort(new Error("boom"));
		expect(track.closed.peek()).toBeNull();

		requests.close();
		broadcast.close();
	}
});

// A fill's answer is no feed arrival: one that fails leaves the gap the live feed still owes holding
// the end and the ordered reader, as the Rust track records arrivals only for the groups its feed
// delivers and serves no failed fetch-only slot. Mirrors Rust `a_failed_fill_answer_holds_an_ordered_reader`.
test("fill: a failed answer to a gap the feed owes holds the end and the ordered reader", async () => {
	for (const error of [new StreamError(StreamCode.Cancel), new Error("archive failed")]) {
		await failedAnswerHoldsTheEnd(error);
	}
});

async function failedAnswerHoldsTheEnd(error: Error) {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { maxAge: null }));
	hooks.receiveTrack(track);
	const reader = track.subscribe(withMax({ delay: { max: null } }));
	const ordered = reader.ordered();
	for (const key of [0n, 2n]) hooks.receiveGroup(track, { start: key, offset: 0 }).finish();
	expect((await ordered.nextGroup())?.start).toBe(0n);
	const requests = track.requests();
	const fetching = wireOf(broadcast).fetchGroup("video", 1n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	defined(fill.accept(1n)).abort(error);
	fill.close();
	await fetching.catch(() => undefined);
	hooks.endTrack(track, { group: 3n, frame: 0 });
	await yieldNow();
	expect(track.closed.peek()).toBeUndefined();

	// A subscriber arriving after the failure is not handed the failed answer either.
	const late = track.subscribe(withMax({ delay: { max: null } }));
	const lateOrdered = late.ordered();
	expect((await lateOrdered.nextGroup())?.start).toBe(0n);
	const lateNext = lateOrdered.nextGroup();
	expect(await Promise.race([lateNext.then(() => "moved"), yieldNow().then(() => "waiting")])).toBe("waiting");

	// The failed answer is no verdict on group 1, so an ordered reader still waits for it rather
	// than moving on to group 2, and gets it once the feed delivers it.
	const next = ordered.nextGroup();
	expect(await Promise.race([next.then(() => "moved"), yieldNow().then(() => "waiting")])).toBe("waiting");
	hooks.receiveGroup(track, { start: 1n, offset: 0 }).finish();
	expect((await next)?.start).toBe(1n);
	expect((await ordered.nextGroup())?.start).toBe(2n);
	expect((await lateNext)?.start).toBe(1n);

	reader.close();
	late.close();
	requests.close();
	broadcast.close();
}

// A reader whose range lowers the copy's floor asks the live feed for those groups, as the update
// it sends does, so none is asked of a fill too, even before the session declares the new floor:
// the START answered the old floor, so it no longer bounds what the feed owes.
test("fill: a group a lowered floor asks the feed for is not asked of a fill", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { maxAge: null }));
	hooks.receiveTrack(track);
	const live = track.subscribe(
		withMax({ ranges: [{ start: { live: { min: Duration.ZERO, max: Duration.ZERO } } }], delay: { max: null } }),
	);
	hooks.startTrack(track, { start: 1n });
	hooks.receiveGroup(track, { start: 1n, offset: 0 }).finish();
	expect((await live.recvGroup())?.start).toBe(1n);
	const requests = track.requests();
	const asked = requests.requestedFill();

	const history = track.subscribe(
		withMax({
			ranges: [{ start: 0n, end: 0n }],
			delay: { max: null },
		}),
	);
	const next = history.recvGroup();
	expect(await Promise.race([asked.then(() => "asked"), yieldNow().then(() => "waiting")])).toBe("waiting");
	hooks.receiveGroup(track, { start: 0n, offset: 0 }).finish();
	expect((await next)?.start).toBe(0n);

	live.close();
	history.close();
	requests.close();
	broadcast.close();
});

// Withheld as it fails, so a read in the same turn cannot take it either.
test("fill: a failed answer is withheld from a subscribed reader that reads at once", async () => {
	for (const dispositions of [false, true]) {
		const broadcast = new BroadcastProducer();
		const track = defined(broadcast.createTrack("video", { maxAge: null }));
		hooks.receiveTrack(track);
		hooks.receiveGroup(track, { start: 2n, offset: 0 }).finish();
		const reader = track.subscribe(withMax({ ranges: [{ start: 0n }], delay: { max: null } }));
		const requests = track.requests();
		const fetching = wireOf(broadcast).fetchGroup("video", 1n);
		const fill = await requests.requestedFill();
		if (!fill) throw new Error("no fill of groups");
		defined(fill.accept(1n)).abort(new Error("archive failed"));
		const seen: unknown[] = [];
		for (let recv = hooks.tryRecvGroup(reader, dispositions); recv.kind !== "idle"; ) {
			seen.push(recv.kind === "group" ? recv.group.start : recv);
			recv = hooks.tryRecvGroup(reader, dispositions);
		}
		expect(seen).toEqual([2n]);
		fill.close();
		await fetching.catch(() => undefined);
		reader.close();
		requests.close();
		broadcast.close();
	}
});

// A failed fill answer leaves the cache, but a reader already holding it keeps what it buffered and
// sees the fill's own failure, never a cache eviction.
test("fill: a held answer that fails keeps its frames and its failure", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { maxAge: null }));
	const requests = track.requests();
	const fetching = wireOf(broadcast).fetchGroup("video", 1n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	const answer = defined(fill.accept(1n));
	answer.writeFrame({ payload: enc.encode("fill 1"), timestamp: Timestamp.unscaled(answer.start) });
	const group = await fetching;
	const failure = new Error("archive failed");
	answer.abort(failure);
	fill.close();
	await yieldNow();

	expect(dec.decode((await group.readFrame())?.payload)).toBe("fill 1");
	await expect(group.readFrame()).rejects.toBe(failure);

	requests.close();
	broadcast.close();
});

// A reader judges each sequence once, so one it judged held and then lost is judged again: a fill
// answer that fails after a probe saw it is given up, never asked again, since a source failing
// for good would fail every retry. Mirrors the Rust `Misses::next_filled`.
test("fill: an answer that fails after a probe judged it is given up", async () => {
	const { broadcast, track } = resumed(4n);
	const requests = track.requests();
	const reader = track.subscribe(
		withMax({
			ranges: [{ start: 0n, end: 5n }],
			delay: { max: null },
		}),
	);
	const ordered = reader.ordered();
	const first = ordered.nextGroup();
	const fill = await requests.requestedFill();
	expect(fill && [fill.start, fill.end]).toEqual([0n, 4n]);
	if (!fill) throw new Error("no fill of groups");
	defined(fill.accept(0n)).finish();
	const answer = defined(fill.accept(2n));
	answer.writeFrame({ payload: enc.encode("fill 2"), timestamp: Timestamp.unscaled(answer.start) });
	expect((await first)?.start).toBe(0n);
	// Waiting on group 1, the bounded window probes everything it names, group 2 held among it.
	const second = ordered.nextGroup();
	await yieldNow();
	answer.abort(new Error("archive failed"));
	defined(fill.accept(1n)).finish();
	expect((await second)?.start).toBe(1n);

	const third = ordered.nextGroup();
	const again = await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)]);
	expect(again && [again.start, again.end]).toBeUndefined();
	defined(fill.accept(3n)).finish();
	expect((await third)?.start).toBe(3n);

	fill.close();
	reader.close();
	requests.close();
	broadcast.close();
});

// A gap a probe judged stale by its successor is judged again once that successor fails, past a
// failed group below it, which was no successor.
test("fill: a gap below a group that fails after a probe is asked", async () => {
	const broadcast = new BroadcastProducer();
	// The publisher's max age bounds history, so a superseded gap goes stale at once.
	const track = defined(broadcast.createTrack("video", { maxAge: Duration.ZERO }));
	const requests = track.requests();
	const reader = track.subscribe({ ranges: [{ start: 0n }] });
	const ordered = reader.ordered();
	const failed = track.createGroup(2n);
	const group = track.createGroup(4n);
	group.writeFrame({ payload: enc.encode("live 4"), timestamp: Timestamp.unscaled(group.start) });
	failed.abort(new StreamError(StreamCode.Cancel));
	await yieldNow();
	// Judged stale against group 4, so not asked.
	hooks.sourceStart(reader);
	group.abort(new StreamError(StreamCode.Cancel));
	await yieldNow();
	const next = ordered.nextGroup();
	const fill = await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)]);
	expect(fill && [fill.start, fill.end]).toEqual([0n, 2n]);

	fill?.close();
	reader.close();
	requests.close();
	broadcast.close();
	await next.catch(() => undefined);
});

// What lay above the newest group when a probe judged it is judged again once the feed passes it.
test("fill: a gap the newest group rises over after a probe is asked", async () => {
	const { broadcast, track } = resumed(2n);
	const requests = track.requests();
	const reader = track.subscribe(withMax({ ranges: [{ start: 2n }], delay: { max: null } }));
	expect(reader.tryRecvGroup()?.start).toBe(2n);
	expect(reader.tryRecvGroup()).toBeUndefined();
	const group = track.createGroup(5n);
	group.writeFrame({ payload: enc.encode("live 5"), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
	expect(reader.tryRecvGroup()?.start).toBe(5n);
	reader.tryRecvGroup();
	const fill = await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)]);
	expect(fill && [fill.start, fill.end]).toEqual([3n, 5n]);

	fill?.close();
	reader.close();
	requests.close();
	broadcast.close();
});

// A miss judged while a fill was out but no handler open is asked once one opens.
test("fill: a miss judged with no handler open is asked once one opens", async () => {
	const { broadcast, track } = resumed(2n);
	const first = track.requests();
	const reader = track.subscribe(withMax({ ranges: [{ start: 0n }], delay: { max: null } }));
	expect(reader.tryRecvGroup()?.start).toBe(2n);
	reader.tryRecvGroup();
	const out = await first.requestedFill();
	expect(out && [out.start, out.end]).toEqual([0n, 2n]);
	first.close();
	const group = track.createGroup(5n);
	group.finish();
	expect(reader.tryRecvGroup()?.start).toBe(5n);
	reader.tryRecvGroup();

	const second = track.requests();
	reader.tryRecvGroup();
	const fill = await Promise.race([second.requestedFill(), yieldNow().then(() => undefined)]);
	expect(fill && [fill.start, fill.end]).toEqual([3n, 5n]);

	fill?.close();
	out?.close();
	reader.close();
	second.close();
	broadcast.close();
});

test("fill: a reader reaching back for history gets the misses filled in order", async () => {
	const { broadcast, track } = resumed(4n);
	const requests = track.requests();
	const fills = respond(
		requests,
		new Map([
			[0n, ["g0"]],
			[1n, ["g1"]],
			[3n, ["g3"]],
		]),
	);

	const reader = track.subscribe(history);
	const ordered = reader.ordered();
	const seen: [bigint, string[]][] = [];
	for (let i = 0; i < 4; i++) {
		const group = await ordered.nextGroup();
		if (!group) break;
		seen.push([group.start, await frames(group)]);
	}
	// Group 2 is missing from the archive, so it is dropped and the cursor moves past it.
	expect(seen).toEqual([
		[0n, ["g0"]],
		[1n, ["g1"]],
		[3n, ["g3"]],
		[4n, ["live 4"]],
	]);
	expect(fills.map((fill) => [fill.start, fill.end])).toEqual([[0n, 4n]]);

	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a bounded reader ends once its fill answers every group", async () => {
	const { broadcast, track } = resumed(4n);
	const requests = track.requests();
	respond(requests, new Map([[1n, ["g1"]]]));

	const reader = track.subscribe(
		withMax({
			ranges: [{ start: 0n, end: 2n }],
			delay: { max: null },
		}),
	);
	const got: bigint[] = [];
	for (let group = await reader.recvGroup(); group; group = await reader.recvGroup()) got.push(group.start);
	expect(got).toEqual([1n]);

	requests.close();
	broadcast.close();
});

test("fill: a reader at the live edge asks nothing", async () => {
	const { broadcast, track } = resumed(4n);
	const requests = track.requests();
	const reader = track.subscribe();
	expect((await reader.recvGroup())?.start).toBe(4n);
	const queued = await Promise.race([requests.requestedFill(), yieldNow().then(() => "none")]);
	expect(queued).toBe("none");

	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: closing a fill drops what is still wanted, and closing the handler drops what is queued", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const a = wireOf(broadcast).fetchGroup("video", 1n);
	const fill = await requests.requestedFill();
	fill?.close();
	await expect(a).rejects.toBeInstanceOf(NotFound);

	const b = wireOf(broadcast).fetchGroup("video", 3n);
	await yieldNow();
	requests.close();
	await expect(b).rejects.toBeInstanceOf(NotFound);
	// The last handler left, so a miss fails at once again.
	await expect(wireOf(broadcast).fetchGroup("video", 2n)).rejects.toBeInstanceOf(NotFound);

	broadcast.close();
});

test("fill: a refusal fails the readers waiting now, and a later reader asks again", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const waiting = wireOf(broadcast).fetchGroup("video", 2n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	const refusal = new StreamError(StreamCode.Cancel, { message: "storage busy" });
	fill.refuseGroups(2n, 3n, refusal);
	fill.close();
	await expect(waiting).rejects.toBe(refusal);

	const later = wireOf(broadcast).fetchGroup("video", 2n);
	const again = await requests.requestedFill();
	expect(again?.start).toBe(2n);
	if (again) serve(again, new Map([[2n, ["ok"]]]));
	expect(await frames(await later)).toEqual(["ok"]);

	requests.close();
	broadcast.close();
});

test("fill: a group arriving after a refusal reaches the reader the refusal named", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const waiting = wireOf(broadcast).fetchGroup("video", 2n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	fill.refuseGroups(2n, 3n, new StreamError(StreamCode.Cancel, { message: "storage busy" }));
	// Before the waiting fetch looks again.
	const group = track.createGroup(2n);
	group.writeFrame({ payload: enc.encode("late"), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
	fill.close();
	expect(await frames(await waiting)).toEqual(["late"]);

	requests.close();
	broadcast.close();
});

test("fill: the track's skip of a run a reader's fill refused is what the reader reports", async () => {
	const { broadcast, track } = resumed(4n);
	const requests = track.requests();
	const reader = track.subscribe(
		withMax({
			ranges: [{ start: 0n, end: 2n }],
			delay: { max: null },
		}),
	);
	const first = reader.recvGroup();
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	fill.refuseGroups(0n, 2n, new StreamError(StreamCode.Cancel, { message: "storage busy" }));
	// The publisher declares the refused run never produced; its verdict wins, as in Rust.
	track.skipGroups(0n, 2n);
	expect(hooks.tryDrop(reader)).toMatchObject({ kind: "drop", start: 0n, end: 2n, skip: true });

	fill.close();
	requests.close();
	broadcast.close();
	await first.catch(() => undefined);
});

test("fill: a fill names the most urgent priority among its readers", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const low = track.subscribe(withMax({ ...history, priority: 1, delay: { max: Duration.millis(60_000) } }));
	void drain(low);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect([fill.start, fill.end]).toEqual([0n, 5n]);
	expect(fill.priority).toBe(1);
	expect(fill.order).toBe("ascending");

	const high = track.subscribe({ ...history, priority: 7, order: "descending" });
	void drain(high);
	await yieldNow();
	expect(fill.priority).toBe(7);
	expect(fill.order).toBe("descending");
	expect(fill.wanted()).toBe(4n);

	low.close();
	high.close();
	await yieldNow();
	expect(fill.wanted()).toBeUndefined();

	fill.close();
	requests.close();
	broadcast.close();
});

// A stale verdict settles only the readers it covers, so a fetch it does not settle keeps its fill.
// Rust records a stale run without answering any fill.
test("fill: a stale verdict leaves an outstanding fill owed", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { maxAge: null }));
	const requests = track.requests();
	track.createGroup(3n).finish();
	const fetching = wireOf(broadcast).fetchGroup("video", 2n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect(fill.wanted()).toBe(2n);

	hooks.dropTrackGroups(track, 2n, 3n, staleHole(500));
	expect(fill.wanted()).toBe(2n);
	const group = defined(fill.accept(2n));
	group.writeFrame({ payload: enc.encode("filled"), timestamp: Timestamp.unscaled(group.start) });
	group.finish();
	expect(await frames(await fetching)).toEqual(["filled"]);

	fill.close();
	requests.close();
	broadcast.close();
});

test("fill: a group arriving live answers the fill", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { maxAge: null }));
	const requests = track.requests();
	const late = track.createGroup(3n);
	late.finish();
	const fetching = wireOf(broadcast).fetchGroup("video", 1n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");

	// The producer writes it itself before the handler gets to it.
	const live = track.createGroup(1n);
	live.writeFrame({ payload: enc.encode("live"), timestamp: Timestamp.unscaled(live.start) });
	live.finish();
	expect(fill.wanted()).toBeUndefined();
	expect(fill.accept(1n)).toBeUndefined();
	expect(await frames(await fetching)).toEqual(["live"]);

	fill.close();
	requests.close();
	broadcast.close();
});

test("fill: an ordered reader stops waiting on an unanswered fill once the max age convicts the hole", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		// The publisher's max age bounds history, so it convicts the holes below group 5.
		const { broadcast, track } = resumed(5n, Duration.millis(1000));
		const requests = track.requests();
		const reader = track.subscribe({ ranges: [{ start: 0n }], order: "ascending" });
		const next = reader.ordered().nextGroup();
		// The handler takes the fill for the missing groups and never answers.
		const fill = await requests.requestedFill();
		expect([fill?.start, fill?.end]).toEqual([0n, 5n]);

		// Group 5 arrived when the reader asked, so the max age gives up the holes below a second on.
		await clock.advance(Duration.millis(1001));
		expect((await next)?.start).toBe(5n);

		fill?.close();
		reader.close();
		requests.close();
		broadcast.close();
	} finally {
		installed.close();
	}
});

test("fill: an arrival-order reader stops waiting on an unanswered fill once the max age convicts the hole", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const { broadcast, track } = resumed(5n, Duration.millis(1000));
		track.finish();
		const requests = track.requests();
		const reader = track.subscribe({ ranges: [{ start: 0n }] });
		expect((await reader.recvGroup())?.start).toBe(5n);
		const next = reader.recvGroup();
		const fill = await requests.requestedFill();
		expect([fill?.start, fill?.end]).toEqual([0n, 5n]);

		// The fill never answers; the holes go past the max age, and the finished track ends the read.
		await clock.advance(Duration.millis(1001));
		expect(await next).toBeUndefined();
		// The reader gave the stale holes up, so the handler hears it no longer waits on them.
		expect(fill?.wanted()).toBeUndefined();
		expect(fill?.isWanted(0n)).toBe(false);

		fill?.close();
		requests.close();
		broadcast.close();
	} finally {
		installed.close();
	}
});

test("fill: a handler hears that a reader stopped wanting what it handed out past", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const { broadcast, track } = resumed(5n, Duration.millis(1000));
		const requests = track.requests();
		const reader = track.subscribe({ ranges: [{ start: 0n }], order: "ascending" });
		const next = reader.ordered().nextGroup();
		const fill = await requests.requestedFill();
		if (!fill) throw new Error("no fill of groups");
		expect(await fill.wantedChanged()).toBe(0n);
		const changed = fill.wantedChanged();
		const unwanted = fill.unwanted(0n);

		// Past the max age the reader takes group 5, leaving the holes below it for good.
		await clock.advance(Duration.millis(1001));
		expect((await next)?.start).toBe(5n);
		expect(await changed).toBeUndefined();
		await unwanted;
		expect(fill.isWanted(0n)).toBe(false);

		fill.close();
		reader.close();
		requests.close();
		broadcast.close();
	} finally {
		installed.close();
	}
});

test("fill: a bounded reader keeps waiting on a fresh fill once an older one goes stale", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const broadcast = new BroadcastProducer();
		const track = defined(broadcast.createTrack("video", { maxAge: null }));
		const write = (key: bigint) => {
			const group = track.createGroup(key);
			group.writeFrame({
				payload: enc.encode(`live ${key}`),
				timestamp: Timestamp.unscaled(group.start),
			});
			group.finish();
		};
		// Holes 0-1 wait on group 2, which arrives first; holes 3-4 on group 5, half a second later.
		write(2n);
		await clock.advance(Duration.millis(500));
		write(5n);
		const requests = track.requests();
		const reader = track.subscribe(
			withMax({ order: "ascending", ranges: [{ start: 0n, end: 5n }], delay: { max: Duration.millis(1000) } }),
		);
		expect((await reader.recvGroup())?.start).toBe(2n);
		expect((await reader.recvGroup())?.start).toBe(5n);
		const next = reader.recvGroup();
		const fills: FillGroups[] = [];
		for (let i = 0; i < 2; i++) {
			const fill = await requests.requestedFill();
			if (fill) fills.push(fill);
		}
		expect(fills.map((fill) => [fill.start, fill.end])).toEqual([
			[0n, 2n],
			[3n, 5n],
		]);

		// Holes 0-1 are past the budget now, but 3-4 are not: the read waits on them.
		await clock.advance(Duration.millis(600));
		const late = defined(fills[1].accept(3n));
		late.writeFrame({ payload: enc.encode("old 3"), timestamp: Timestamp.unscaled(late.start) });
		late.finish();
		expect((await next)?.start).toBe(3n);

		for (const fill of fills) fill.close();
		expect(await reader.recvGroup()).toBeUndefined();
		requests.close();
		broadcast.close();
	} finally {
		installed.close();
	}
});

test("fill: a reader joining a wider fill is owed only its own ranges", async () => {
	const { broadcast, track } = resumed(10n);
	const requests = track.requests();
	const wide = track.subscribe(
		withMax({
			ranges: [{ start: 0n, end: 9n }],
			delay: { max: null },
			order: "ascending",
		}),
	);
	void drain(wide);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect([fill.start, fill.end]).toEqual([0n, 10n]);

	// A narrower reader joins the same fill for 2 and 7 alone.
	const narrow = track.subscribe(
		withMax({
			ranges: [
				{ start: 2n, end: 2n },
				{ start: 7n, end: 7n },
			],
			delay: { max: null },
			order: "ascending",
		}),
	);
	const got: bigint[] = [];
	const done = (async () => {
		for (let group = await narrow.recvGroup(); group; group = await narrow.recvGroup()) {
			got.push(group.start);
			group.close();
		}
	})();
	// Its time starts precede every group held, so each asks for the group containing it, one key
	// wide and floored at 0, but it asks no other fill.
	const looked: FillGroups[] = [];
	for (let i = 0; i < 2; i++) {
		const lookup = await requests.requestedFill();
		if (!lookup) throw new Error("no time start");
		expect([lookup.floor, lookup.end - lookup.start]).toEqual([0n, 1n]);
		looked.push(lookup);
	}
	expect(looked.map((lookup) => lookup.start).sort()).toEqual([2n, 7n]);
	await yieldNow();
	expect(await Promise.race([requests.requestedFill(), yieldNow().then(() => "none")])).toBe("none");

	for (const key of [2n, 7n]) {
		const group = defined(fill.accept(key));
		group.writeFrame({ payload: enc.encode(`old ${key}`), timestamp: Timestamp.unscaled(group.start) });
		group.finish();
	}
	// Each start's group landed through the wide fill, which answers it.
	for (const lookup of looked) expect(lookup.wanted()).toBeUndefined();
	// The wide reader still waits on the rest of the fill; the narrow one has all it reads.
	await done;
	expect(got).toEqual([2n, 7n]);
	expect(fill.wanted()).toBe(0n);

	fill.close();
	wide.close();
	requests.close();
	broadcast.close();
});

test("fill: a group no reader reads any more stays withdrawn, and widening back asks a new fill", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	// Narrowed by its end: a time start before every group held would wait on a lookup instead.
	const ranges = (end: bigint) => [{ start: 0n, end: end }];
	const reader = track.subscribe(withMax({ ranges: ranges(4n), delay: { max: null }, order: "ascending" }));
	void drain(reader);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect([fill.start, fill.end]).toEqual([0n, 5n]);

	reader.update(withMax({ ranges: ranges(2n), delay: { max: null }, order: "ascending" }));
	expect(fill.isWanted(3n)).toBe(false);
	expect(fill.wanted()).toBe(0n);

	// Widening back does not revive the group in this fill: the handler may have stopped on it.
	reader.update(withMax({ ranges: ranges(4n), delay: { max: null }, order: "ascending" }));
	await yieldNow();
	expect(fill.isWanted(3n)).toBe(false);
	const again = await requests.requestedFill();
	expect([again?.start, again?.end]).toEqual([3n, 5n]);

	// Each narrowing and widening back asks anew.
	reader.update(withMax({ ranges: ranges(2n), delay: { max: null }, order: "ascending" }));
	reader.update(withMax({ ranges: ranges(4n), delay: { max: null }, order: "ascending" }));
	await yieldNow();
	expect(again?.isWanted(3n)).toBe(false);
	const third = await requests.requestedFill();
	expect([third?.start, third?.end]).toEqual([3n, 5n]);

	third?.close();
	again?.close();
	fill.close();
	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: what a reader's sequence cursor passes, a fill no longer owes it", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const reader = track.subscribe(history);
	// A logical reader that already holds what lies below the floor raises its copy's cursor.
	expect(hooks.tryNextGroup(reader, 0n).kind).toBe("idle");
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect(fill.wanted()).toBe(0n);
	const changed = fill.wantedChanged();
	await changed;
	const next = fill.wantedChanged();

	hooks.tryNextGroup(reader, 3n);
	expect(await next).toBe(3n);
	expect(fill.isWanted(1n)).toBe(false);

	fill.close();
	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a filled group stays wanted only while a reader will still read it", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	// A live reader already passed every group below the edge.
	const live = track.subscribe(
		withMax({ ranges: [{ start: { live: { min: Duration.ZERO, max: Duration.ZERO } } }], delay: { max: null } }),
	);
	expect((await live.ordered().nextGroup())?.start).toBe(5n);

	const reader = track.subscribe(history);
	const next = reader.ordered().nextGroup();
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect(fill.wanted()).toBe(0n);
	const source = defined(fill.accept(0n));
	source.writeFrame({ payload: enc.encode("g0"), timestamp: Timestamp.unscaled(source.start) });
	const taken = await next;
	expect(taken?.start).toBe(0n);

	// A reader still to reach it keeps it wanted once the one that took it lets go.
	const behind = track.subscribe(history);
	taken?.close();
	reader.close();
	// Closing a reader releases its copy a microtask later.
	await yieldNow();
	const wanted = source.demand().wanted;
	expect(wanted.peek()).toBe(true);

	// Narrowed past it, nobody reads it: only the live reader's copy is left, which it passed.
	behind.update({ ...history, ranges: [{ start: 3n }] });
	expect(wanted.peek()).toBe(false);
	// Widened back over it, it counts again.
	behind.update(history);
	expect(wanted.peek()).toBe(true);
	behind.close();
	await yieldNow();
	expect(wanted.peek()).toBe(false);

	source.finish();
	fill.close();
	live.close();
	requests.close();
	broadcast.close();
});

test("fill: a reader changing only its order wakes the handler", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const reader = track.subscribe(history);
	void drain(reader);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect(await fill.wantedChanged()).toBe(0n);
	const changed = fill.wantedChanged();

	reader.update({ ...history, order: "descending" });
	expect(await changed).toBe(4n);
	expect(fill.order).toBe("descending");

	fill.close();
	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a reader parked on a miss asks once a handler opens", async () => {
	const { broadcast, track } = resumed(5n);
	const reader = track.subscribe(history);
	// It reads the live group and parks for the next; nobody could take its miss below.
	expect((await reader.recvGroup())?.start).toBe(5n);
	const next = reader.recvGroup();
	await yieldNow();

	// A handler opening wakes it to ask.
	const requests = track.requests();
	const fill = await requests.requestedFill();
	expect([fill?.start, fill?.end]).toEqual([0n, 5n]);
	if (fill) serve(fill, new Map([[0n, ["old 0"]]]));
	expect((await next)?.start).toBe(0n);

	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a reader joins a fill still served after its handler's handle closed", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const first = wireOf(broadcast).fetchGroup("video", 2n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	requests.close();

	// The fill is still out, so a second fetch joins it rather than failing at once.
	const second = wireOf(broadcast).fetchGroup("video", 2n);
	await yieldNow();
	serve(fill, new Map([[2n, ["shared"]]]));
	expect(await frames(await first)).toEqual(["shared"]);
	expect(await frames(await second)).toEqual(["shared"]);
	// With the fill answered and no handle open, a new miss fails at once again.
	await expect(wireOf(broadcast).fetchGroup("video", 3n)).rejects.toBeInstanceOf(NotFound);

	broadcast.close();
});

test("fill: a group the track gives up elsewhere answers the fill", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const fetching = wireOf(broadcast).fetchGroup("video", 2n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect(fill.wanted()).toBe(2n);

	// An upstream SUBSCRIBE_DROP, say: the handler is no longer asked for it.
	const cut = new StreamError(StreamCode.Cancel, { message: "cut upstream" });
	hooks.dropTrackGroups(track, 2n, 3n, { error: cut });
	expect(fill.wanted()).toBeUndefined();
	await expect(fetching).rejects.toBe(cut);

	fill.close();
	requests.close();
	broadcast.close();
});

// A received stale run that does not settle a reader is unknown to it, so its fill is still asked:
// here the upstream judged it under an open range, while the reader is capped. Mirrors the Rust
// `Misses::unclaimed`.
test("fill: a stale run that does not settle the reader is still asked for", async () => {
	const { broadcast, track } = resumed(2n);
	const requests = track.requests();
	hooks.dropTrackGroups(track, 0n, 2n, staleHole(60_000));
	const reader = track.subscribe(
		withMax({
			ranges: [{ start: 0n, end: 1n }],
			delay: { max: Duration.millis(500) },
		}),
	);
	void drain(reader);
	const fill = await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)]);
	expect(fill && [fill.start, fill.end]).toEqual([0n, 2n]);

	fill?.close();
	reader.close();
	requests.close();
	broadcast.close();
});

// The stale runs above the newest group a reader cannot trust are memoized, and what the track
// gives up after a probe is judged on the next: a run given up later is asked too. Mirrors the
// Rust `Misses::unknown`.
test("fill: a stale run given up after a probe is asked on the next", async () => {
	const { broadcast, track } = resumed(2n);
	const requests = track.requests();
	hooks.dropTrackGroups(track, 5n, 7n, staleHole(60_000));
	const reader = track.subscribe(
		withMax({
			ranges: [{ start: 2n, end: 19n }],
			delay: { max: Duration.millis(500) },
		}),
	);
	expect(reader.tryRecvGroup()?.start).toBe(2n);
	reader.tryRecvGroup();
	const asked = async () => {
		const fills: bigint[][] = [];
		for (;;) {
			const fill = await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)]);
			if (!fill) return fills;
			fills.push([fill.start, fill.end]);
			fill.close();
		}
	};
	expect(await asked()).toEqual([[5n, 7n]]);

	hooks.dropTrackGroups(track, 9n, 10n, staleHole(60_000));
	reader.tryRecvGroup();
	expect(await asked()).toEqual([[9n, 10n]]);

	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a stale run that does not settle the reader leaves its own refusal reported", async () => {
	const { broadcast, track } = resumed(2n);
	const requests = track.requests();
	hooks.dropTrackGroups(track, 0n, 2n, staleHole(60_000));
	const reader = track.subscribe(
		withMax({
			ranges: [{ start: 0n, end: 1n }],
			delay: { max: Duration.millis(500) },
		}),
	);
	const first = reader.recvGroup();
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	const refusal = new StreamError(StreamCode.Cancel, { message: "storage busy" });
	fill.refuseGroups(0n, 2n, refusal);
	expect(hooks.tryDrop(reader)).toMatchObject({ kind: "drop", start: 0n, end: 2n, error: refusal });

	fill.close();
	requests.close();
	broadcast.close();
	await first.catch(() => undefined);
});

test("fill: a stale run that does not settle the reader keeps its pending refusals", async () => {
	const { broadcast, track } = resumed(6n);
	const requests = track.requests();
	const reader = track.subscribe(
		withMax({
			ranges: [{ start: 0n, end: 5n }],
			delay: { max: Duration.millis(500) },
		}),
	);
	const first = reader.recvGroup();
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	const refusal = new StreamError(StreamCode.Cancel, { message: "storage busy" });
	fill.refuseGroups(0n, 1n, refusal);
	fill.refuseGroups(3n, 4n, refusal);
	expect(hooks.tryDrop(reader)).toMatchObject({ start: 0n, end: 1n, error: refusal });
	// Folded in as a change, over the refusal still to report.
	hooks.dropTrackGroups(track, 2n, 5n, staleHole(60_000));
	expect(hooks.tryDrop(reader)).toMatchObject({ start: 3n, end: 4n, error: refusal });

	fill.close();
	requests.close();
	broadcast.close();
	await first.catch(() => undefined);
});

test("fill: a cache-miss fetch of a dynamic track serves the one track the broadcast already has", async () => {
	const broadcast = new BroadcastProducer();
	// Pulled first, so the broadcast serves tracks on demand.
	const pulled = wireOf(broadcast).requested();
	const fetching = wireOf(broadcast).fetchGroup("video", 1n);
	const request = await pulled;
	if (!request) throw new Error("no track request");
	const track = request.accept({ maxAge: null });
	const requests = track.requests();
	const group = track.createGroup(4n);
	group.finish();

	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	serve(fill, new Map([[1n, ["one"]]]));
	expect(await frames(await fetching)).toEqual(["one"]);

	// A second miss and a live reader land on the same producer, not a second request.
	const second = wireOf(broadcast).fetchGroup("video", 2n);
	const queued = await Promise.race([wireOf(broadcast).requested(), yieldNow().then(() => "none")]);
	expect(queued).toBe("none");
	const refill = await requests.requestedFill();
	if (refill) serve(refill, new Map([[2n, ["two"]]]));
	expect(await frames(await second)).toEqual(["two"]);

	requests.close();
	broadcast.close();
});

// A browser publisher's history, read by a peer over a session: lite FETCH on every lite version
// with one, and lite-07's ranged SUBSCRIBE.
const url = new URL("https://localhost:4443/test");

async function session(alpn: string) {
	const pair = createMockTransportPair(alpn);
	const origin = new OriginProducer();
	const [client, server] = await Promise.all([
		connectSession({ url, transport: pair.client }),
		acceptSession({ transport: pair.server, url, publish: origin.consume() }),
	]);
	const publication = origin.route(Path.from("archive"));
	const broadcast = publication.broadcast;
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	// Groups 0 to 2 were published before this producer existed.
	const live = track.createGroup(3n);
	live.writeFrame({ payload: enc.encode("live 3"), timestamp: Timestamp.fromMillis(3000) });
	live.finish();
	const requests = track.requests();
	const fills: FillGroups[] = [];
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			fills.push(fill);
			for (let key = fill.wanted(); key !== undefined; key = fill.wanted()) {
				if (key === 1n) {
					fill.dropGroups(1n, 2n);
					continue;
				}
				const group = defined(fill.accept(key));
				group.writeFrame({
					payload: enc.encode(`old ${key}`),
					timestamp: Timestamp.fromMillis(Number(key) * 1000),
				});
				group.finish();
			}
			fill.close();
		}
	})();
	const remote = wireOf(client).consume(Path.from("archive"));
	return {
		remote,
		fills,
		close() {
			remote.close();
			requests.close();
			broadcast.close();
			client.abort();
			server.abort();
			origin.close();
		},
	};
}

test.each([Lite.ALPN_05, Lite.ALPN_06, Lite.ALPN_07_WIP])(
	"fill: %s FETCH reads a group the publisher never cached, and NotFound for one it lacks",
	async (alpn) => {
		const peer = await session(alpn);
		try {
			const group = await peer.remote.track("video").fetchGroup(2n);
			const frame = await group.readFrame();
			expect(dec.decode(frame?.payload)).toBe("old 2");
			expect(frame?.timestamp?.asMillis()).toBe(2000);
			expect(await group.readFrame()).toBeUndefined();

			const missing = await peer.remote
				.track("video")
				.fetchGroup(1n)
				.then(
					() => undefined,
					(err: unknown) => err,
				);
			expect(toStreamCode(missing)).toBe(StreamCode.NotFound);
		} finally {
			peer.close();
		}
	},
);

test("fill: a lite-07 ranged subscription reads history the publisher never cached", async () => {
	const peer = await session(Lite.ALPN_07_WIP);
	try {
		const reader = peer.remote.track("video").subscribe(
			withMax({
				ranges: [{ start: 0n, end: 3n }],
				delay: { max: null },
				order: "ascending",
			}),
		);
		const got: [bigint, string[]][] = [];
		for (let group = await reader.recvGroup(); group; group = await reader.recvGroup()) {
			got.push([group.start, await frames(group)]);
		}
		got.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
		expect(got).toEqual([
			[0n, ["old 0"]],
			[2n, ["old 2"]],
			[3n, ["live 3"]],
		]);
		reader.close();
	} finally {
		peer.close();
	}
});

// A fill's OLD answer under the publisher's max age binds every reader, so the track holds it as
// a stale verdict under that age rather than a loss for good: the readers waiting now are refused,
// a later reader no wider is settled without asking, and once new terms forget the verdict a
// reader asks again. Mirrors the Rust `a_fills_old_answer_under_the_max_age_is_held_until_forgotten`.
test("fill: an OLD answer under the max age is held as stale until forgotten", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { maxAge: Duration.millis(10_000) }));
	const live = track.createGroup(3n);
	live.writeFrame({ payload: enc.encode("live 3"), timestamp: Timestamp.unscaled(live.start) });
	live.finish();
	const requests = track.requests();
	const terms: Subscription = withMax({
		ranges: [{ start: 0n }],
		delay: { max: Duration.millis(60_000) },
		order: "ascending",
	});
	const asked = async () => {
		const fills: bigint[][] = [];
		for (;;) {
			const fill = await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)]);
			if (!fill) return fills;
			fills.push([fill.start, fill.end]);
			fill.close();
		}
	};
	const reader = track.subscribe(terms);
	expect(reader.tryRecvGroup()?.start).toBe(3n);
	reader.tryRecvGroup();
	// A reader of the run alone, which the verdict does not settle, waits on the same fill.
	const bounded = track.subscribe({ ...terms, ranges: [{ start: 0n, end: 2n }] });
	bounded.tryRecvGroup();
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill of groups");
	expect([fill.start, fill.end]).toEqual([0n, 3n]);
	const old = new StreamError(StreamCode.Old, { message: "old" });
	rejectGroups(fill, 0n, 3n, old, 10_000);
	expect(hooks.tryDrop(bounded)).toMatchObject({ kind: "drop", start: 0n, end: 3n, error: old });
	fill.close();

	const later = track.subscribe(terms);
	expect(later.tryRecvGroup()?.start).toBe(3n);
	later.tryRecvGroup();
	expect(hooks.tryDrop(later)).toMatchObject({ kind: "drop", start: 0n, end: 3n, stale: 10_000 });
	expect(await asked()).toEqual([]);

	hooks.supersedeStale(track);
	const again = track.subscribe(terms);
	expect(again.tryRecvGroup()?.start).toBe(3n);
	again.tryRecvGroup();
	expect(await asked()).toEqual([[0n, 3n]]);

	reader.close();
	bounded.close();
	later.close();
	again.close();
	requests.close();
	broadcast.close();
});

// A track holding group 2 from frame 3 only, as a copy that joined it mid-group does, and group 3
// whole.
function headless(): { broadcast: BroadcastProducer; track: Producer } {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { maxAge: null }));
	const tail = hooks.receiveGroup(track, { start: 2n, offset: 3 });
	for (const frame of ["c3", "c4"])
		tail.writeFrame({ payload: enc.encode(frame), timestamp: Timestamp.unscaled(2n) });
	tail.finish();
	const next = track.createGroup(3n);
	next.writeFrame({ payload: enc.encode("d0"), timestamp: Timestamp.unscaled(3n) });
	next.finish();
	return { broadcast, track };
}

const whole: Archive = new Map([[2n, ["c0", "c1", "c2", "c3", "c4"]]]);
const twoAndThree: Subscription = withMax({
	ranges: [{ start: 2n, end: 3n }],
	delay: { max: null },
	order: "ascending",
});

test.each(["arrival", "sequence"])(
	"fill: a bounded %s reader hands out a headless group once a fill answers it from frame 0",
	async (mode) => {
		const { broadcast, track } = headless();
		const requests = track.requests();
		const fills = respond(requests, whole);

		const reader = track.subscribe(twoAndThree);
		const next =
			mode === "arrival"
				? () => reader.recvGroup()
				: (() => {
						const ordered = reader.ordered();
						return () => ordered.nextGroup();
					})();
		const got: [bigint, string[]][] = [];
		for (let group = await next(); group; group = await next()) got.push([group.start, await frames(group)]);
		got.sort(([a], [b]) => (a < b ? -1 : 1));
		expect(got).toEqual([
			[2n, ["c0", "c1", "c2", "c3", "c4"]],
			[3n, ["d0"]],
		]);
		expect(fills.map((fill) => [fill.start, fill.end])).toEqual([[2n, 3n]]);

		// An open reader takes the whole copy that replaced the headless one in the cache.
		const open = track.subscribe(history);
		const first = await open.ordered().nextGroup();
		expect([first?.start, first && (await frames(first))]).toEqual([2n, ["c0", "c1", "c2", "c3", "c4"]]);

		open.close();
		requests.close();
		broadcast.close();
	},
);

test("fill: a refused head gives its group up, and the bounded reader moves past it", async () => {
	const { broadcast, track } = headless();
	const requests = track.requests();
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			fill.refuseGroups(fill.start, fill.end, new NotFound("no head"));
			fill.close();
		}
	})();

	const reader = track.subscribe(twoAndThree);
	const ordered = reader.ordered();
	const got: bigint[] = [];
	for (let group = await ordered.nextGroup(); group; group = await ordered.nextGroup()) got.push(group.start);
	expect(got).toEqual([3n]);

	requests.close();
	broadcast.close();
});

test.each([
	["arrival", "drop"],
	["arrival", "close"],
	["sequence", "drop"],
	["sequence", "close"],
])("fill: a %s reader asks a head once and gives it up when its fill ends in a %s", async (mode, end) => {
	const { broadcast, track } = headless();
	const requests = track.requests();
	let asked = 0;
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			// A reader asking again would loop forever: refuse past a few, so the test still ends.
			if (++asked > 3) fill.refuseGroups(fill.start, fill.end, new NotFound("asked again"));
			else if (end === "drop") fill.dropGroups(fill.start, fill.end);
			fill.close();
		}
	})();

	const reader = track.subscribe(twoAndThree);
	const ordered = mode === "sequence" ? reader.ordered() : undefined;
	const next = () => (ordered ? ordered.nextGroup() : reader.recvGroup());
	const got: bigint[] = [];
	for (let group = await next(); group; group = await next()) got.push(group.start);
	expect(got).toEqual([3n]);
	expect(asked).toBe(1);

	requests.close();
	broadcast.close();
});

test("fill: a drop of a held group landing while its head is asked leaves the head to its fill", async () => {
	const { broadcast, track } = headless();
	const requests = track.requests();
	const ordered = track.subscribe(twoAndThree).ordered();
	const first = ordered.nextGroup();
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no head asked");
	expect([fill.start, fill.end]).toEqual([2n, 3n]);

	// A drop the feed sent before the head was asked, as a late SUBSCRIBE_DROP, lands now.
	hooks.dropTrackGroups(track, 0n, 3n, { error: new NotFound("dropped") });
	serve(fill, whole);
	const group = await first;
	expect([group?.start, group && (await frames(group))]).toEqual([2n, ["c0", "c1", "c2", "c3", "c4"]]);

	requests.close();
	broadcast.close();
});

test("fill: with no handler, a bounded reader gives a headless group up while an open one reads it", async () => {
	const { broadcast, track } = headless();

	const bounded = track.subscribe(twoAndThree);
	const got: bigint[] = [];
	for (let group = await bounded.recvGroup(); group; group = await bounded.recvGroup()) got.push(group.start);
	expect(got).toEqual([3n]);

	// A bounded fetch from frame 0 fails as a miss no handler serves.
	const fetched = await wireOf(broadcast)
		.fetchGroup("video", 2n)
		.then(
			() => undefined,
			(err: unknown) => err,
		);
	expect(toStreamCode(fetched)).toBe(StreamCode.NotFound);

	const open = track.subscribe(history);
	const first = await open.ordered().nextGroup();
	expect([first?.start, first && (await frames(first))]).toEqual([2n, ["c3", "c4"]]);

	open.close();
	broadcast.close();
});

test("fill: a fetch of a headless group asks its head, as a fetch from frame 0 does", async () => {
	const { broadcast, track } = headless();
	const requests = track.requests();
	const fills = respond(requests, whole);

	const group = await wireOf(broadcast).fetchGroup("video", 2n);
	expect(await frames(group)).toEqual(["c0", "c1", "c2", "c3", "c4"]);
	expect(fills.map((fill) => [fill.start, fill.end])).toEqual([[2n, 3n]]);

	requests.close();
	broadcast.close();
});

// A timed track that cached only group 5, starting at 5ms; its archive holds groups 0 to 4, group
// `n` keyed by its start, `n` milliseconds.
function timed(): { broadcast: BroadcastProducer; track: Producer } {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const group = track.createGroup(5n);
	group.writeFrame({ payload: enc.encode("g5"), timestamp: Timestamp.fromMillis(5) });
	group.finish();
	return { broadcast, track };
}

// Answer each time start with `lookup(time)`, dropping it when that is undefined, as a handler whose
// index holds none there does, and serve every other fill from the archive. Returns the time starts'
// times, in milliseconds.
function lookups(requests: Requests, lookup: (time: bigint) => bigint | undefined): bigint[] {
	const times: bigint[] = [];
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			if (fill.floor < fill.start) {
				times.push(fill.start);
				const answer = lookup(fill.start);
				if (answer === undefined) fill.dropGroups(fill.start, fill.end);
				else {
					const group = defined(fill.accept(answer));
					group.writeFrame({
						payload: enc.encode(`g${answer}`),
						timestamp: Timestamp.fromMillis(Number(answer)),
					});
					group.finish();
				}
				fill.close();
				continue;
			}
			for (let key = fill.wanted(); key !== undefined; key = fill.wanted()) {
				const group = defined(fill.accept(key));
				group.writeFrame({
					payload: enc.encode(`g${key}`),
					timestamp: Timestamp.fromMillis(Number(key)),
				});
				group.finish();
			}
			fill.close();
		}
	})();
	return times;
}

const fromTime = (ms: bigint): Subscription =>
	withMax({
		ranges: [{ start: ms }],
		delay: { max: null },
		order: "ascending",
	});

test("fill: a time start no held group resolves is looked up, and the range starts at the answer", async () => {
	const { broadcast, track } = timed();
	const requests = track.requests();
	const times = lookups(requests, (ms) => ms);

	const reader = track.subscribe(fromTime(2n));
	const ordered = reader.ordered();
	const got: bigint[] = [];
	for (let i = 0; i < 4; i++) got.push((await ordered.nextGroup())?.start ?? -1n);
	expect(got).toEqual([2n, 3n, 4n, 5n]);
	expect(times).toEqual([2n]);

	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a lookup answered below the eviction watermark starts the range there", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const broadcast = new BroadcastProducer();
		const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
		const requests = track.requests();
		const times = lookups(requests, (ms) => ms);
		for (let n = 0n; n < 3n; n++) {
			const group = track.createGroup(n);
			group.writeFrame({ payload: enc.encode(`g${n}`), timestamp: Timestamp.fromMillis(Number(n)) });
			group.finish();
		}
		// Idle past the cache window, so the next write evicts every group but the new edge: the
		// answer and the fills below it land at or below the watermark, unverified.
		clock.jump(Duration.millis(DEFAULT_EXPIRY.asMillis() + 1000));
		const edge = track.createGroup(5n);
		edge.writeFrame({ payload: enc.encode("g5"), timestamp: Timestamp.fromMillis(5) });
		edge.finish();

		const reader = track.subscribe(fromTime(2n));
		const ordered = reader.ordered();
		const got: bigint[] = [];
		for (let i = 0; i < 4; i++) got.push((await ordered.nextGroup())?.start ?? -1n);
		expect(got).toEqual([2n, 3n, 4n, 5n]);
		expect(times).toEqual([2n]);

		reader.close();
		requests.close();
		broadcast.close();
	} finally {
		installed.close();
	}
});

// As in Rust: a key a reader is owed is never lost to the cache, so an answer evicted before the
// reader takes it is asked again however often, never given up after one refill.
test("fill: an answer evicted again before its reader takes it is asked again", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const broadcast = new BroadcastProducer();
		const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
		const requests = track.requests();
		let edge = 4n;
		const write = (group: GroupProducer) => {
			group.writeFrame({
				payload: enc.encode(`g${group.start}`),
				timestamp: Timestamp.fromMillis(Number(group.start)),
			});
			group.finish();
		};
		// Idle past the cache window, so the next write evicts every group but the new edge.
		const evict = () => {
			clock.jump(Duration.millis(DEFAULT_EXPIRY.asMillis() + 1000));
			write(track.createGroup(++edge));
		};
		for (let n = 0n; n <= edge; n++) write(track.createGroup(n));
		evict();
		const reader = track.subscribe({ ...history, ranges: [{ start: 0n, end: 4n }] });
		const ordered = reader.ordered();
		const next = ordered.nextGroup();
		const fill = defined(await requests.requestedFill());
		expect([fill.start, fill.end]).toEqual([0n, 5n]);

		// The reader waits on group 0 while the answer for 3 is evicted, twice over.
		write(defined(fill.accept(3n)));
		for (let round = 0; round < 2; round++) {
			evict();
			await yieldNow();
			const again = defined(await requests.requestedFill());
			expect([again.start, again.end]).toEqual([3n, 4n]);
			write(defined(again.accept(3n)));
			again.close();
		}
		expect(hooks.tryDrop(reader)).toBeUndefined();
		for (let key = fill.wanted(); key !== undefined; key = fill.wanted()) write(defined(fill.accept(key)));
		fill.close();
		const got = [(await next)?.start];
		for (let n = 1; n < 5; n++) got.push((await ordered.nextGroup())?.start);
		expect(got).toEqual([0n, 1n, 2n, 3n, 4n]);

		reader.close();
		requests.close();
		broadcast.close();
	} finally {
		installed.close();
	}
});

// A time in the pause past a whole group starts where it would with that group cached: the handler
// answers with the group, the reader waits for it to end, and its End places the time past it, so
// the group is never read. Mirrors the Rust `a_looked_up_group_ending_before_the_time_starts_past_it`.
test("fill: a looked-up group ending before the time starts past it", async () => {
	for (const cached of [true, false]) {
		const broadcast = new BroadcastProducer();
		const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
		const requests = track.requests();
		const write = (group: ReturnType<typeof track.createGroup>, start: number) =>
			group.writeFrame({ payload: enc.encode(`g${start}`), timestamp: Timestamp.fromMillis(start) });
		const times: bigint[] = [];
		void (async () => {
			for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
				// A miss past 1000, or a time start floored past it: the index holds nothing there.
				if (fill.floor < fill.start) times.push(fill.start, fill.floor);
				if (fill.floor > 1000n) {
					fill.dropGroups(fill.start, fill.end);
					fill.close();
					continue;
				}
				const group = defined(fill.accept(1000n));
				write(group, 1000);
				await yieldNow();
				group.finishAt(1400n);
				fill.close();
			}
		})();
		for (const [start, end] of [
			[0, 400n],
			[1000, 1400n],
			[3000, 3400n],
		] as const) {
			if (start === 1000 && !cached) continue;
			const group = track.createGroup(BigInt(start));
			write(group, start);
			group.finishAt(end);
		}
		const reader = track.subscribe(fromTime(1700n));
		expect((await reader.ordered().nextGroup())?.start).toBe(3000n);
		// From the End before the time, where a group may start.
		expect(times).toEqual(cached ? [1700n, 1400n] : [1700n, 400n]);
		reader.close();
		requests.close();
		broadcast.close();
	}
});

// A looked-up group still open is read once it holds a frame past the time, which puts its End past
// the time too. Mirrors the Rust `a_looked_up_group_with_a_frame_past_the_time_is_read_before_it_ends`.
test("fill: a looked-up group with a frame past the time is read before it ends", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const requests = track.requests();
	const write = (group: ReturnType<typeof track.createGroup>, at: number) =>
		group.writeFrame({ payload: enc.encode(`f${at}`), timestamp: Timestamp.fromMillis(at) });
	let answered: ReturnType<typeof track.createGroup> | undefined;
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			if (fill.floor > 1000n) {
				fill.dropGroups(fill.start, fill.end);
				fill.close();
				continue;
			}
			answered = defined(fill.accept(1000n));
			fill.close();
		}
	})();
	for (const start of [0, 3000]) {
		const held = track.createGroup(BigInt(start));
		write(held, start);
		held.finishAt(BigInt(start + 400));
	}
	const reader = track.subscribe(fromTime(1300n));
	const next = reader.ordered().nextGroup();
	while (answered === undefined) await yieldNow();
	const group = answered;
	let read: bigint | undefined;
	void next.then((got) => {
		read = got?.start;
	});
	for (const at of [1000, 1300]) {
		write(group, at);
		for (let i = 0; i < 10; i++) await yieldNow();
		expect(read).toBeUndefined();
	}
	write(group, 1350);
	expect((await next)?.start).toBe(1000n);
	group.finishAt(1400n);
	reader.close();
	requests.close();
	broadcast.close();
});

// A looked-up group whose frames before the time outgrow its retained tail sheds its head before the
// reader may take it. With its handler gone, nothing serves that head again, so the read fails rather
// than waiting or delivering short. Mirrors the Rust
// `a_looked_up_group_shedding_its_head_before_the_time_fails_the_read`.
test("fill: a looked-up group shedding its head before the time fails the read", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const requests = track.requests();
	let answered: ReturnType<typeof track.createGroup> | undefined;
	void (async () => {
		const fill = await requests.requestedFill();
		if (!fill) return;
		answered = defined(fill.accept(1000n));
		fill.close();
		requests.close();
	})();
	for (const start of [0, 3000]) {
		const held = track.createGroup(BigInt(start));
		held.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(start) });
		held.finishAt(BigInt(start + 400));
	}
	const reader = track.subscribe(fromTime(1300n));
	const ordered = reader.ordered();
	const next = ordered.nextGroup();
	while (answered === undefined) await yieldNow();
	const group = answered;
	// Past the retained tail, all at or before the time.
	const chunk = new Uint8Array(1 << 20);
	for (let i = 0; i < 34; i++) group.writeFrame({ payload: chunk, timestamp: Timestamp.fromMillis(1000) });
	group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1350) });
	group.finishAt(1400n);
	const read = defined(await next);
	expect(read.start).toBe(1000n);
	await expect(read.readFrame()).rejects.toThrow();
	reader.close();
	broadcast.close();
});

// A time past a held group's End is looked up from that End itself: a group the cache doesn't hold
// may start there, and its own span places the time. Mirrors the Rust
// `a_time_past_a_held_groups_end_looks_up_a_group_starting_there`.
test("fill: a time past a held group's End looks up a group starting there", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const requests = track.requests();
	const write = (group: ReturnType<typeof track.createGroup>, at: number) =>
		group.writeFrame({ payload: enc.encode(`f${at}`), timestamp: Timestamp.fromMillis(at) });
	const asked: bigint[][] = [];
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			asked.push([fill.start, fill.floor]);
			// The index holds [1000, 1400), which a fill from above 1000 can't take.
			if (fill.floor <= 1000n && 1000n < fill.end) {
				const group = defined(fill.accept(1000n));
				write(group, 1000);
				group.finishAt(1400n);
			} else fill.dropGroups(fill.floor, fill.end);
			fill.close();
		}
	})();
	const first = track.appendGroup(Timestamp.fromMillis(0));
	write(first, 0);
	first.finishAt(1000n);
	const later = track.createGroup(3000n);
	write(later, 3000);
	later.finishAt(3400n);
	const reader = track.subscribe(fromTime(1700n));
	expect((await reader.ordered().nextGroup())?.start).toBe(3000n);
	expect(asked[0]).toEqual([1700n, 1000n]);
	reader.close();
	requests.close();
	broadcast.close();
});

// A lookup no group answers from its floor places the time by the span of the group the floor is
// past, waiting for that group to reach the time. Mirrors the Rust
// `a_dropped_lookup_places_the_time_by_the_group_below_its_floor`.
test("fill: a dropped lookup places the time by the group below its floor", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const requests = track.requests();
	const write = (group: ReturnType<typeof track.createGroup>, at: number) =>
		group.writeFrame({ payload: enc.encode(`f${at}`), timestamp: Timestamp.fromMillis(at) });
	const floors: bigint[] = [];
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			floors.push(fill.floor);
			fill.dropGroups(fill.floor, fill.end);
			fill.close();
		}
	})();
	const open = track.createGroup(1000n);
	write(open, 1000);
	const later = track.createGroup(3000n);
	write(later, 3000);
	later.finishAt(3400n);
	const reader = track.subscribe(fromTime(1700n));
	const next = reader.ordered().nextGroup();
	let read: bigint | undefined;
	void next.then((got) => {
		read = got?.start;
	});
	for (let i = 0; i < 20; i++) await yieldNow();
	expect(floors).toEqual([1001n]);
	expect(read).toBeUndefined();
	open.finishAt(1400n);
	expect((await next)?.start).toBe(3000n);
	reader.close();
	requests.close();
	broadcast.close();
});

// A history handler pacing on its readers writes the group a lookup waits on: the lookup reads it,
// so neither waits on the other. Mirrors the Rust
// `a_looked_up_group_is_read_while_its_writer_paces_on_readers`.
test("fill: a looked-up group is read while its writer paces on readers", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const requests = track.requests();
	const write = (group: ReturnType<typeof track.createGroup>, at: number) =>
		group.writeFrame({ payload: enc.encode(`f${at}`), timestamp: Timestamp.fromMillis(at) });
	let answered: ReturnType<typeof track.createGroup> | undefined;
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			if (fill.floor > 1000n) {
				fill.dropGroups(fill.start, fill.end);
				fill.close();
				continue;
			}
			answered = defined(fill.accept(1000n));
			fill.close();
		}
	})();
	for (const start of [0, 3000]) {
		const held = track.createGroup(BigInt(start));
		write(held, start);
		held.finishAt(BigInt(start + 400));
	}
	const reader = track.subscribe(fromTime(1300n));
	const next = reader.ordered().nextGroup();
	while (answered === undefined) await yieldNow();
	const group = answered;
	for (const at of [1000, 1350]) {
		for (let i = 0; i < 20 && !group.demand().wanted.peek(); i++) await yieldNow();
		expect([at, group.demand().wanted.peek()]).toEqual([at, true]);
		write(group, at);
	}
	expect((await next)?.start).toBe(1000n);
	group.finishAt(1400n);
	reader.close();
	requests.close();
	broadcast.close();
});

// A looked-up group aborting before it reaches the time proves nothing of where it ends, so the read
// fails rather than start at it. Mirrors the Rust
// `a_looked_up_group_aborting_before_the_time_fails_the_read`.
test("fill: a looked-up group aborting before the time fails the read", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const requests = track.requests();
	let answered: ReturnType<typeof track.createGroup> | undefined;
	void (async () => {
		const fill = await requests.requestedFill();
		if (!fill) return;
		answered = defined(fill.accept(1000n));
		fill.close();
	})();
	for (const start of [0, 3000]) {
		const held = track.createGroup(BigInt(start));
		held.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(start) });
		held.finishAt(BigInt(start + 400));
	}
	const reader = track.subscribe(fromTime(1700n));
	const next = reader.ordered().nextGroup();
	while (answered === undefined) await yieldNow();
	answered.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1000) });
	const abort = new StreamError(StreamCode.Cancel, { message: "upstream gone" });
	answered.abort(abort);
	await expect(next).rejects.toBe(abort);
	reader.close();
	requests.close();
	broadcast.close();
});

// A looked-up group streaming one frame past its retained tail before the time is placed by its later
// frames, then fails the read with its shed head: the probe moves past frames without building their
// payloads, so it never throws on the frame whose head is gone. Mirrors the Rust
// `a_looked_up_group_shedding_its_head_before_the_time_fails_the_read`.
test("fill: a looked-up group streaming a frame past its tail before the time fails the read", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const requests = track.requests();
	let answered: ReturnType<typeof track.createGroup> | undefined;
	void (async () => {
		const fill = await requests.requestedFill();
		if (!fill) return;
		answered = defined(fill.accept(1000n));
		fill.close();
		requests.close();
	})();
	for (const start of [0, 3000]) {
		const held = track.createGroup(BigInt(start));
		held.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(start) });
		held.finishAt(BigInt(start + 400));
	}
	const reader = track.subscribe(fromTime(1300n));
	const next = reader.ordered().nextGroup();
	while (answered === undefined) await yieldNow();
	const group = answered;
	const chunk = new Uint8Array(1 << 20);
	const chunks = 34;
	const frame = group.createFrame({ size: chunk.length * chunks, timestamp: Timestamp.fromMillis(1000) });
	for (let i = 0; i < chunks; i++) {
		frame.write(chunk);
		await yieldNow();
	}
	frame.finish();
	group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1350) });
	group.finishAt(1400n);
	const read = defined(await next);
	expect(read.start).toBe(1000n);
	await expect(read.readFrame()).rejects.toThrow();
	reader.close();
	broadcast.close();
});

// A subscriber closing mid-lookup lets its probe go, so the looked-up group is no longer wanted.
test("fill: a subscriber closing mid-lookup stops wanting the looked-up group", async () => {
	const broadcast = new BroadcastProducer();
	const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
	const requests = track.requests();
	let answered: ReturnType<typeof track.createGroup> | undefined;
	void (async () => {
		const fill = await requests.requestedFill();
		if (!fill) return;
		answered = defined(fill.accept(1000n));
		fill.close();
	})();
	for (const start of [0, 3000]) {
		const held = track.createGroup(BigInt(start));
		held.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(start) });
		held.finishAt(BigInt(start + 400));
	}
	const reader = track.subscribe(fromTime(1700n));
	const next = reader.ordered().nextGroup();
	while (answered === undefined) await yieldNow();
	const group = answered;
	group.writeFrame({ payload: enc.encode("x"), timestamp: Timestamp.fromMillis(1000) });
	for (let i = 0; i < 20 && !group.demand().wanted.peek(); i++) await yieldNow();
	expect(group.demand().wanted.peek()).toBe(true);
	reader.close();
	await next.catch(() => undefined);
	for (let i = 0; i < 20 && group.demand().wanted.peek(); i++) await yieldNow();
	expect(group.demand().wanted.peek()).toBe(false);
	group.finishAt(1400n);
	requests.close();
	broadcast.close();
});

test("fill: a time start dropped after its floor's group is evicted starts at that group", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const broadcast = new BroadcastProducer();
		const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
		const requests = track.requests();
		const write = (n: bigint) => {
			const group = track.createGroup(n);
			group.writeFrame({ payload: enc.encode(`g${n}`), timestamp: Timestamp.fromMillis(Number(n)) });
			group.finish();
		};
		write(1n);
		write(9n);
		const reader = track.subscribe(
			withMax({
				ranges: [{ start: 7n, end: 7n }],
				delay: { max: null },
				order: "ascending",
			}),
		);
		const next = reader.ordered().nextGroup();
		const first = defined(await requests.requestedFill());
		expect([first.start, first.floor]).toEqual([7n, 2n]);
		// Idle past the cache window, so the next write evicts every group but the new edge.
		clock.jump(Duration.millis(DEFAULT_EXPIRY.asMillis() + 1000));
		write(10n);
		// The index says 1 holds the time, below the floor: nothing from 2 does. Mirrors the Rust
		// `a_time_start_is_answered_by_the_group_below_it`: the reader starts at 1 whatever the cache
		// holds now, asking it back as a miss rather than the time again.
		first.dropGroups(2n, 8n);
		first.close();
		const miss = defined(await requests.requestedFill());
		expect([miss.start, miss.floor]).toEqual([1n, 1n]);
		const found = defined(miss.accept(1n));
		found.writeFrame({ payload: enc.encode("g1"), timestamp: Timestamp.fromMillis(1) });
		found.finish();
		miss.dropGroups(2n, 8n);
		miss.close();
		expect((await next)?.start).toBe(1n);

		reader.close();
		requests.close();
		broadcast.close();
	} finally {
		installed.close();
	}
});

test("fill: a lookup's answer is read at once, before its first frame: its key is its start", async () => {
	const { broadcast, track } = timed();
	const requests = track.requests();
	const reader = track.subscribe(fromTime(2n));
	const next = reader.ordered().nextGroup();
	// The handler answers with group 2 but writes no frame yet.
	const lookup = await requests.requestedFill();
	if (!lookup) throw new Error("no time start");
	expect([lookup.start, lookup.floor]).toEqual([2n, 0n]);
	const found = defined(lookup.accept(2n));
	expect((await next)?.start).toBe(2n);

	found.finish();
	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a time start dropped from 0 starts the range at its time, asking nothing below it", async () => {
	const { broadcast, track } = timed();
	const requests = track.requests();
	const times = lookups(requests, () => undefined);

	// Mirrors the Rust `a_time_start_before_the_cache_asks_nothing_below_it`: no group starts at or
	// before the time, so the range reads from it, and the archive fills from 3, past the key dropped.
	const reader = track.subscribe(fromTime(2n));
	const ordered = reader.ordered();
	const got: bigint[] = [];
	for (let i = 0; i < 3; i++) got.push((await ordered.nextGroup())?.start ?? -1n);
	expect(got).toEqual([3n, 4n, 5n]);
	expect(times).toEqual([2n]);

	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a time start just past a held group, dropped, starts at that group", async () => {
	// Mirrors the Rust `a_time_start_just_past_a_held_group_drops_to_it`: the floor is the time
	// itself, so the fill asks only its own key, and its drop starts the reader at group 4, which the
	// walk passed while the lookup was out.
	for (const ordered of [false, true]) {
		const broadcast = new BroadcastProducer();
		const track = defined(broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null }));
		for (const n of [4n, 10n]) {
			const group = track.createGroup(n);
			group.writeFrame({ payload: enc.encode(`g${n}`), timestamp: Timestamp.fromMillis(Number(n)) });
			group.finish();
		}
		const requests = track.requests();
		const floors: bigint[][] = [];
		void (async () => {
			for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
				floors.push([fill.start, fill.floor]);
				fill.dropGroups(fill.start, fill.end);
				fill.close();
			}
		})();
		const reader = track.subscribe(fromTime(5n));
		const got: bigint[] = [];
		if (ordered) {
			const keys = reader.ordered();
			for (let i = 0; i < 2; i++) got.push((await keys.nextGroup())?.start ?? -1n);
		} else for (let i = 0; i < 2; i++) got.push((await reader.recvGroup())?.start ?? -1n);
		expect(got).toEqual([4n, 10n]);
		expect(floors[0]).toEqual([5n, 5n]);

		reader.close();
		requests.close();
		broadcast.close();
	}
});

test("fill: a refused time start fails its reader until its ranges change", async () => {
	const { broadcast, track } = timed();
	const requests = track.requests();
	// Mirrors the Rust `a_refused_time_start_fails_its_reader`.
	const reader = track.subscribe(fromTime(2n));
	const ordered = reader.ordered();
	const next = ordered.nextGroup();
	const start = defined(await requests.requestedFill());
	expect([start.start, start.floor]).toEqual([2n, 0n]);
	const refusal = new Error("store down");
	start.refuseGroups(start.start, start.end, refusal);
	start.close();
	// Not group 5, which the cache holds past the time.
	await expect(next).rejects.toBe(refusal);
	await expect(ordered.nextGroup()).rejects.toBe(refusal);

	// An edit replacing it resolves every range again, each by its Range ID.
	const times = lookups(requests, (ms) => ms);
	hooks.editSubscriber(reader, {
		retire: [0n],
		add: [[1n, { start: 3n }]],
		priority: 0,
		order: "ascending",
	});
	const got: bigint[] = [];
	for (let i = 0; i < 3; i++) got.push((await ordered.nextGroup())?.start ?? -1n);
	expect(got).toEqual([3n, 4n, 5n]);
	expect(times).toEqual([3n]);
	expect(hooks.servingResolvedKeys(reader, [1n])[0]?.start).toBe(3n);

	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a time start the held groups place asks no lookup", async () => {
	const { broadcast, track } = timed();
	const requests = track.requests();
	const times = lookups(requests, () => 0n);

	const reader = track.subscribe(fromTime(6n));
	expect((await reader.ordered().nextGroup())?.start).toBe(5n);
	expect(times).toEqual([]);

	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: replacing the ranges withdraws a lookup still out", async () => {
	const { broadcast, track } = timed();
	const requests = track.requests();
	const reader = track.subscribe(fromTime(2n));
	expect(reader.tryRecvGroup()).toBeUndefined();
	const lookup = await requests.requestedFill();
	if (!lookup) throw new Error("no time start");
	expect(lookup.start).toBe(2n);

	reader.update({ ranges: [{ start: 5n }], order: "ascending" });
	expect(reader.tryRecvGroup()?.start).toBe(5n);
	// Nobody waits on time 2 now.
	expect(lookup.wanted()).toBeUndefined();

	reader.close();
	requests.close();
	broadcast.close();
});
