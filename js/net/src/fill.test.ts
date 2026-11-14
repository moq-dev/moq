import { expect, test } from "bun:test";
import { Duration, yieldNow } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import { accept as acceptSession, connect as connectSession } from "./connection/index.ts";
import { NotFound, StreamCode, StreamError, toStreamCode } from "./error.ts";
import { rejectGroups } from "./fill.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import { hooks, staleHole } from "./internal.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Timescale, Timestamp } from "./time.ts";
import type { Fill, Producer, Requests, Subscription } from "./track.ts";
import { wireOf } from "./wire.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

// Every track here states no publisher max age: a fill serves history, which the default zero
// max age would leave stale.

// The archive an in-memory responder serves: each group's frames, by sequence.
type Archive = Map<bigint, string[]>;

// Serve every fill from `archive` until the track closes, dropping what it lacks. Returns the
// fills it took, so a test can count them.
function respond(requests: Requests, archive: Archive): Fill[] {
	const fills: Fill[] = [];
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			fills.push(fill);
			serve(fill, archive);
		}
	})();
	return fills;
}

function serve(fill: Fill, archive: Archive): void {
	for (let sequence = fill.wanted(); sequence !== undefined; sequence = fill.wanted()) {
		const frames = archive.get(sequence);
		if (!frames) {
			fill.dropGroups(sequence, sequence + 1n);
			continue;
		}
		const group = fill.accept(sequence);
		for (const frame of frames) group.writeFrame({ payload: enc.encode(frame) });
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
function resumed(first: bigint): { broadcast: BroadcastProducer; track: Producer } {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video", { maxAge: null });
	const group = track.createGroup(first);
	group.writeFrame({ payload: enc.encode(`live ${first}`) });
	group.finish();
	return { broadcast, track };
}

const history: Subscription = { ranges: [{ start: { absolute: 0n } }], delay: { max: null }, order: "ascending" };

test("fill: a fetch of a group never cached is served by the request handler, byte for byte", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const fills = respond(requests, new Map([[2n, ["a", "b", "c"]]]));

	const group = await wireOf(broadcast).fetchGroup("video", 2n);
	expect(group.sequence).toBe(2n);
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
	if (!fill) throw new Error("no fill");
	expect([fill.start, fill.end]).toEqual([7n, 8n]);
	expect(fill.wanted()).toBe(7n);

	// The producer gets there before the handler answers: the live write is the answer.
	const group = track.createGroup(7n);
	group.writeFrame({ payload: enc.encode("written") });
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
	if (!fill) throw new Error("no fill");
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
	if (!fill) throw new Error("no fill");
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
		if (!fill) throw new Error("no fill");
		const group = fill.accept(0n);
		group.writeFrame({ payload: enc.encode("backfill") });
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
	const track = broadcast.createTrack("video", { maxAge: null });
	hooks.receiveTrack(track);
	const reader = track.subscribe({ delay: { max: null } });
	const ordered = reader.ordered();
	for (const sequence of [0n, 2n]) hooks.receiveGroup(track, { sequence, start: 0 }).finish();
	expect((await ordered.nextGroup())?.sequence).toBe(0n);
	const requests = track.requests();
	const fetching = wireOf(broadcast).fetchGroup("video", 1n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	fill.accept(1n).abort(error);
	fill.close();
	await fetching.catch(() => undefined);
	hooks.endTrack(track, { group: 3n, frame: 0 });
	await yieldNow();
	expect(track.closed.peek()).toBeUndefined();

	// A subscriber arriving after the failure is not handed the failed answer either.
	const late = track.subscribe({ delay: { max: null } });
	const lateOrdered = late.ordered();
	expect((await lateOrdered.nextGroup())?.sequence).toBe(0n);
	const lateNext = lateOrdered.nextGroup();
	expect(await Promise.race([lateNext.then(() => "moved"), yieldNow().then(() => "waiting")])).toBe("waiting");

	// The failed answer is no verdict on group 1, so an ordered reader still waits for it rather
	// than moving on to group 2, and gets it once the feed delivers it.
	const next = ordered.nextGroup();
	expect(await Promise.race([next.then(() => "moved"), yieldNow().then(() => "waiting")])).toBe("waiting");
	hooks.receiveGroup(track, { sequence: 1n, start: 0 }).finish();
	expect((await next)?.sequence).toBe(1n);
	expect((await ordered.nextGroup())?.sequence).toBe(2n);
	expect((await lateNext)?.sequence).toBe(1n);

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
	const track = broadcast.createTrack("video", { maxAge: null });
	hooks.receiveTrack(track);
	const live = track.subscribe({ ranges: [{ start: { relative: 0n } }], delay: { max: null } });
	hooks.startTrack(track, { start: 1n });
	hooks.receiveGroup(track, { sequence: 1n, start: 0 }).finish();
	expect((await live.recvGroup())?.sequence).toBe(1n);
	const requests = track.requests();
	const asked = requests.requestedFill();

	const history = track.subscribe({
		ranges: [{ start: { absolute: 0n }, end: { absolute: 1n } }],
		delay: { max: null },
	});
	const next = history.recvGroup();
	expect(await Promise.race([asked.then(() => "asked"), yieldNow().then(() => "waiting")])).toBe("waiting");
	hooks.receiveGroup(track, { sequence: 0n, start: 0 }).finish();
	expect((await next)?.sequence).toBe(0n);

	live.close();
	history.close();
	requests.close();
	broadcast.close();
});

// Withheld as it fails, so a read in the same turn cannot take it either.
test("fill: a failed answer is withheld from a subscribed reader that reads at once", async () => {
	for (const dispositions of [false, true]) {
		const broadcast = new BroadcastProducer();
		const track = broadcast.createTrack("video", { maxAge: null });
		hooks.receiveTrack(track);
		hooks.receiveGroup(track, { sequence: 2n, start: 0 }).finish();
		const reader = track.subscribe({ ranges: [{ start: { absolute: 0n } }], delay: { max: null } });
		const requests = track.requests();
		const fetching = wireOf(broadcast).fetchGroup("video", 1n);
		const fill = await requests.requestedFill();
		if (!fill) throw new Error("no fill");
		fill.accept(1n).abort(new Error("archive failed"));
		const seen: unknown[] = [];
		for (let recv = hooks.tryRecvGroup(reader, dispositions); recv.kind !== "idle"; ) {
			seen.push(recv.kind === "group" ? recv.group.sequence : recv);
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
	const track = broadcast.createTrack("video", { maxAge: null });
	const requests = track.requests();
	const fetching = wireOf(broadcast).fetchGroup("video", 1n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	const answer = fill.accept(1n);
	answer.writeFrame({ payload: enc.encode("fill 1") });
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
// answer that fails after a probe saw it is asked once more. Mirrors the Rust `Misses` inbox.
test("fill: an answer withheld after a probe judged it is asked again", async () => {
	const { broadcast, track } = resumed(4n);
	const requests = track.requests();
	const reader = track.subscribe({
		ranges: [{ start: { absolute: 0n }, end: { absolute: 6n } }],
		delay: { max: null },
	});
	const ordered = reader.ordered();
	const first = ordered.nextGroup();
	const fill = await requests.requestedFill();
	expect(fill && [fill.start, fill.end]).toEqual([0n, 4n]);
	if (!fill) throw new Error("no fill");
	fill.accept(0n).finish();
	const answer = fill.accept(2n);
	answer.writeFrame({ payload: enc.encode("fill 2") });
	expect((await first)?.sequence).toBe(0n);
	// Waiting on group 1, the bounded window probes everything it names, group 2 held among it.
	const second = ordered.nextGroup();
	await yieldNow();
	answer.abort(new Error("archive failed"));
	fill.accept(1n).finish();
	expect((await second)?.sequence).toBe(1n);

	const third = ordered.nextGroup();
	const again = await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)]);
	expect(again && [again.start, again.end]).toEqual([2n, 3n]);
	if (again) serve(again, new Map([[2n, ["again"]]]));
	expect((await third)?.sequence).toBe(2n);

	fill.close();
	reader.close();
	requests.close();
	broadcast.close();
});

// A gap a probe judged stale by its successor is judged again once that successor fails, past a
// failed group below it, which was no successor.
test("fill: a gap below a group that fails after a probe is asked", async () => {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video", { maxAge: null });
	const requests = track.requests();
	const reader = track.subscribe({ ranges: [{ start: { absolute: 0n } }], delay: { max: Duration.millis(0) } });
	const ordered = reader.ordered();
	const failed = track.createGroup(2n);
	const group = track.createGroup(4n);
	group.writeFrame({ payload: enc.encode("live 4") });
	failed.abort(new StreamError(StreamCode.Cancel));
	await yieldNow();
	// Judged stale against group 4, so not asked.
	await reader.start();
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
	const reader = track.subscribe({ ranges: [{ start: { absolute: 2n } }], delay: { max: null } });
	expect(reader.tryRecvGroup()?.sequence).toBe(2n);
	expect(reader.tryRecvGroup()).toBeUndefined();
	const group = track.createGroup(5n);
	group.writeFrame({ payload: enc.encode("live 5") });
	group.finish();
	expect(reader.tryRecvGroup()?.sequence).toBe(5n);
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
	const reader = track.subscribe({ ranges: [{ start: { absolute: 0n } }], delay: { max: null } });
	expect(reader.tryRecvGroup()?.sequence).toBe(2n);
	reader.tryRecvGroup();
	const out = await first.requestedFill();
	expect(out && [out.start, out.end]).toEqual([0n, 2n]);
	first.close();
	const group = track.createGroup(5n);
	group.finish();
	expect(reader.tryRecvGroup()?.sequence).toBe(5n);
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
		seen.push([group.sequence, await frames(group)]);
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

	const reader = track.subscribe({
		ranges: [{ start: { absolute: 0n }, end: { absolute: 3n } }],
		delay: { max: null },
	});
	const got: bigint[] = [];
	for (let group = await reader.recvGroup(); group; group = await reader.recvGroup()) got.push(group.sequence);
	expect(got).toEqual([1n]);

	requests.close();
	broadcast.close();
});

test("fill: a reader at the live edge asks nothing", async () => {
	const { broadcast, track } = resumed(4n);
	const requests = track.requests();
	const reader = track.subscribe();
	expect((await reader.recvGroup())?.sequence).toBe(4n);
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
	if (!fill) throw new Error("no fill");
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
	if (!fill) throw new Error("no fill");
	fill.refuseGroups(2n, 3n, new StreamError(StreamCode.Cancel, { message: "storage busy" }));
	// Before the waiting fetch looks again.
	const group = track.createGroup(2n);
	group.writeFrame({ payload: enc.encode("late") });
	group.finish();
	fill.close();
	expect(await frames(await waiting)).toEqual(["late"]);

	requests.close();
	broadcast.close();
});

test("fill: the track's skip of a run a reader's fill refused is what the reader reports", async () => {
	const { broadcast, track } = resumed(4n);
	const requests = track.requests();
	const reader = track.subscribe({
		ranges: [{ start: { absolute: 0n }, end: { absolute: 3n } }],
		delay: { max: null },
	});
	const first = reader.recvGroup();
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	fill.refuseGroups(0n, 2n, new StreamError(StreamCode.Cancel, { message: "storage busy" }));
	// The publisher declares the refused run never produced; its verdict wins, as in Rust.
	track.skipGroups(0n, 2n);
	expect(hooks.tryDrop(reader)).toMatchObject({ kind: "drop", start: 0n, end: 2n, skip: true });

	fill.close();
	requests.close();
	broadcast.close();
	await first.catch(() => undefined);
});

test("fill: a fill names the most urgent priority and loosest budget among its readers", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const low = track.subscribe({ ...history, priority: 1, delay: { max: Duration.millis(60_000) } });
	void drain(low);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	expect([fill.start, fill.end]).toEqual([0n, 5n]);
	expect(fill.priority).toBe(1);
	expect(fill.maxDelay).toEqual(Duration.millis(60_000));
	expect(fill.order).toBe("ascending");

	const high = track.subscribe({ ...history, priority: 7, order: "descending" });
	void drain(high);
	await yieldNow();
	expect(fill.priority).toBe(7);
	expect(fill.maxDelay).toBeNull();
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
	const track = broadcast.createTrack("video", { maxAge: null });
	const requests = track.requests();
	track.createGroup(3n).finish();
	const fetching = wireOf(broadcast).fetchGroup("video", 2n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	expect(fill.wanted()).toBe(2n);

	hooks.dropTrackGroups(track, 2n, 3n, staleHole(500));
	expect(fill.wanted()).toBe(2n);
	const group = fill.accept(2n);
	group.writeFrame({ payload: enc.encode("filled") });
	group.finish();
	expect(await frames(await fetching)).toEqual(["filled"]);

	fill.close();
	requests.close();
	broadcast.close();
});

test("fill: a group arriving live answers the fill", async () => {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video", { maxAge: null });
	const requests = track.requests();
	const late = track.createGroup(3n);
	late.finish();
	const fetching = wireOf(broadcast).fetchGroup("video", 1n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");

	// The producer writes it itself before the handler gets to it.
	const live = track.createGroup(1n);
	live.writeFrame({ payload: enc.encode("live") });
	live.finish();
	expect(fill.wanted()).toBeUndefined();
	expect(() => fill.accept(1n)).toThrow();
	expect(await frames(await fetching)).toEqual(["live"]);

	fill.close();
	requests.close();
	broadcast.close();
});

test("fill: an ordered reader stops waiting on an unanswered fill once its budget convicts the hole", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const { broadcast, track } = resumed(5n);
		const requests = track.requests();
		const reader = track.subscribe({
			ranges: [{ start: { absolute: 0n } }],
			delay: { max: Duration.millis(1000) },
			order: "ascending",
		});
		const next = reader.ordered().nextGroup();
		// The handler takes the fill for the missing groups and never answers.
		const fill = await requests.requestedFill();
		expect([fill?.start, fill?.end]).toEqual([0n, 5n]);

		// Group 5 arrived when the reader asked, so its budget gives up the holes below a second on.
		await clock.advance(Duration.millis(1001));
		expect((await next)?.sequence).toBe(5n);

		fill?.close();
		reader.close();
		requests.close();
		broadcast.close();
	} finally {
		installed.close();
	}
});

test("fill: an arrival-order reader stops waiting on an unanswered fill once its budget convicts the hole", async () => {
	const clock = new Manual();
	const installed = clock.install();
	try {
		const { broadcast, track } = resumed(5n);
		track.finish();
		const requests = track.requests();
		const reader = track.subscribe({
			ranges: [{ start: { absolute: 0n } }],
			delay: { max: Duration.millis(1000) },
		});
		expect((await reader.recvGroup())?.sequence).toBe(5n);
		const next = reader.recvGroup();
		const fill = await requests.requestedFill();
		expect([fill?.start, fill?.end]).toEqual([0n, 5n]);

		// The fill never answers; the holes go past the budget, and the finished track ends the read.
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
		const { broadcast, track } = resumed(5n);
		const requests = track.requests();
		const reader = track.subscribe({
			ranges: [{ start: { absolute: 0n } }],
			delay: { max: Duration.millis(1000) },
			order: "ascending",
		});
		const next = reader.ordered().nextGroup();
		const fill = await requests.requestedFill();
		if (!fill) throw new Error("no fill");
		expect(await fill.wantedChanged()).toBe(0n);
		const changed = fill.wantedChanged();
		const unwanted = fill.unwanted(0n);

		// Past the budget the reader takes group 5, leaving the holes below it for good.
		await clock.advance(Duration.millis(1001));
		expect((await next)?.sequence).toBe(5n);
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
		const track = broadcast.createTrack("video", { maxAge: null });
		const write = (sequence: bigint) => {
			const group = track.createGroup(sequence);
			group.writeFrame({ payload: enc.encode(`live ${sequence}`) });
			group.finish();
		};
		// Holes 0-1 wait on group 2, which arrives first; holes 3-4 on group 5, half a second later.
		write(2n);
		await clock.advance(Duration.millis(500));
		write(5n);
		const requests = track.requests();
		const reader = track.subscribe({
			ranges: [{ start: { absolute: 0n }, end: { absolute: 6n } }],
			delay: { max: Duration.millis(1000) },
		});
		expect((await reader.recvGroup())?.sequence).toBe(2n);
		expect((await reader.recvGroup())?.sequence).toBe(5n);
		const next = reader.recvGroup();
		const fills: Fill[] = [];
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
		const late = fills[1].accept(3n);
		late.writeFrame({ payload: enc.encode("old 3") });
		late.finish();
		expect((await next)?.sequence).toBe(3n);

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
	const wide = track.subscribe({
		ranges: [{ start: { absolute: 0n }, end: { absolute: 10n } }],
		delay: { max: null },
		order: "ascending",
	});
	void drain(wide);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	expect([fill.start, fill.end]).toEqual([0n, 10n]);

	// A narrower reader joins the same fill for 2 and 7 alone.
	const narrow = track.subscribe({
		ranges: [
			{ start: { absolute: 2n }, end: { absolute: 3n } },
			{ start: { absolute: 7n }, end: { absolute: 8n } },
		],
		delay: { max: null },
		order: "ascending",
	});
	const got: bigint[] = [];
	const done = (async () => {
		for (let group = await narrow.recvGroup(); group; group = await narrow.recvGroup()) {
			got.push(group.sequence);
			group.close();
		}
	})();
	await yieldNow();
	expect(await Promise.race([requests.requestedFill(), yieldNow().then(() => "none")])).toBe("none");

	for (const sequence of [2n, 7n]) {
		const group = fill.accept(sequence);
		group.writeFrame({ payload: enc.encode(`old ${sequence}`) });
		group.finish();
	}
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
	const ranges = (start: bigint) => [{ start: { absolute: start }, end: { absolute: 5n } }];
	const reader = track.subscribe({ ranges: ranges(0n), delay: { max: null }, order: "ascending" });
	void drain(reader);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	expect([fill.start, fill.end]).toEqual([0n, 5n]);

	reader.update({ ranges: ranges(3n), delay: { max: null }, order: "ascending" });
	expect(fill.isWanted(1n)).toBe(false);
	expect(fill.wanted()).toBe(3n);

	// Widening back does not revive the group in this fill: the handler may have stopped on it.
	reader.update({ ranges: ranges(0n), delay: { max: null }, order: "ascending" });
	await yieldNow();
	expect(fill.isWanted(1n)).toBe(false);
	const again = await requests.requestedFill();
	expect([again?.start, again?.end]).toEqual([0n, 3n]);

	// Each narrowing and widening back asks anew, never spending the refill an eviction gets.
	reader.update({ ranges: ranges(3n), delay: { max: null }, order: "ascending" });
	reader.update({ ranges: ranges(0n), delay: { max: null }, order: "ascending" });
	await yieldNow();
	expect(again?.isWanted(1n)).toBe(false);
	const third = await requests.requestedFill();
	expect([third?.start, third?.end]).toEqual([0n, 3n]);

	third?.close();
	again?.close();
	fill.close();
	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a refusal under an old budget is asked afresh on each widening, never spending the eviction refill", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const terms = (max: number): Subscription => ({
		ranges: [{ start: { absolute: 0n }, end: { absolute: 5n } }],
		delay: { max: Duration.millis(max) },
		order: "ascending",
	});
	const reader = track.subscribe(terms(60_000));
	void drain(reader);
	const old = new StreamError(StreamCode.Old, { message: "old" });
	// Each widening past the last refusal's budget asks a new fill for the run.
	for (const budget of [60_000, 120_000, 240_000]) {
		const fill = await requests.requestedFill();
		expect([fill?.start, fill?.end]).toEqual([0n, 5n]);
		if (!fill) throw new Error("no fill");
		rejectGroups(fill, 0n, 5n, old, { budget, everyone: false });
		fill.close();
		reader.update(terms(budget * 2));
	}
	expect(await requests.requestedFill()).toMatchObject({ start: 0n, end: 5n });

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
	if (!fill) throw new Error("no fill");
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
	const live = track.subscribe({ ranges: [{ start: { relative: 0n } }], delay: { max: null } });
	expect((await live.ordered().nextGroup())?.sequence).toBe(5n);

	const reader = track.subscribe(history);
	const next = reader.ordered().nextGroup();
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	expect(fill.wanted()).toBe(0n);
	const source = fill.accept(0n);
	source.writeFrame({ payload: enc.encode("g0") });
	const taken = await next;
	expect(taken?.sequence).toBe(0n);

	// A reader still to reach it keeps it wanted once the one that took it lets go.
	const behind = track.subscribe(history);
	taken?.close();
	reader.close();
	// Closing a reader releases its copy a microtask later.
	await yieldNow();
	const wanted = source.demand().wanted;
	expect(wanted.peek()).toBe(true);

	// Narrowed past it, nobody reads it: only the live reader's copy is left, which it passed.
	behind.update({ ...history, ranges: [{ start: { absolute: 3n } }] });
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
	if (!fill) throw new Error("no fill");
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
	expect((await reader.recvGroup())?.sequence).toBe(5n);
	const next = reader.recvGroup();
	await yieldNow();

	// A handler opening wakes it to ask.
	const requests = track.requests();
	const fill = await requests.requestedFill();
	expect([fill?.start, fill?.end]).toEqual([0n, 5n]);
	if (fill) serve(fill, new Map([[0n, ["old 0"]]]));
	expect((await next)?.sequence).toBe(0n);

	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a reader joins a fill still served after its handler's handle closed", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const first = wireOf(broadcast).fetchGroup("video", 2n);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
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
	if (!fill) throw new Error("no fill");
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
	const reader = track.subscribe({
		ranges: [{ start: { absolute: 0n }, end: { absolute: 2n } }],
		delay: { max: Duration.millis(500) },
	});
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
	const reader = track.subscribe({
		ranges: [{ start: { absolute: 2n }, end: { absolute: 20n } }],
		delay: { max: Duration.millis(500) },
	});
	expect(reader.tryRecvGroup()?.sequence).toBe(2n);
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
	const reader = track.subscribe({
		ranges: [{ start: { absolute: 0n }, end: { absolute: 2n } }],
		delay: { max: Duration.millis(500) },
	});
	const first = reader.recvGroup();
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
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
	const reader = track.subscribe({
		ranges: [{ start: { absolute: 0n }, end: { absolute: 6n } }],
		delay: { max: Duration.millis(500) },
	});
	const first = reader.recvGroup();
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
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

// The upstream judged 0..2 and 4..6 under 500ms, and gave up both of them as stale.
function judgedTwice() {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video", { maxAge: null });
	hooks.receiveTrack(track);
	const requests = track.requests();
	hooks.supersedeStale(track, [
		{ start: 0n, end: 2n },
		{ start: 4n, end: 6n },
	]);
	hooks.dropTrackGroups(track, 0n, 2n, staleHole(500));
	hooks.dropTrackGroups(track, 4n, 6n, staleHole(500));
	const reader = (ranges: [bigint, bigint][]) =>
		track.subscribe({
			ranges: ranges.map(([start, end]) => ({ start: { absolute: start }, end: { absolute: end } })),
			delay: { max: Duration.millis(500) },
		});
	return { broadcast, requests, reader };
}

// A stale verdict settles a reader that reads every range the upstream judged it against, however
// many there are, with nothing received and no fill asked. Mirrors Rust.
test("fill: a stale verdict settles a reader of every range it judged", async () => {
	const { broadcast, requests, reader: subscribe } = judgedTwice();
	const reader = subscribe([
		[0n, 2n],
		[4n, 6n],
	]);
	expect(hooks.tryDrop(reader)).toMatchObject({ start: 0n, end: 2n, stale: 500 });
	expect(hooks.tryDrop(reader)).toMatchObject({ start: 4n, end: 6n, stale: 500 });
	expect(hooks.tryRecvGroup(reader, false)).toEqual({ kind: "done" });
	const fill = await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)]);
	expect(fill).toBeUndefined();

	reader.close();
	requests.close();
	broadcast.close();
});

// A reader whose ranges only partly overlap the ones a stale verdict was judged against is settled
// where it reads every judged group above the run, and asks a fill for the rest, even with nothing
// received: no update brings it, since its ranges add nothing to the upstream's terms. Mirrors Rust.
test("fill: a stale verdict settles a partly overlapping reader only where it reads above", async () => {
	const { broadcast, requests, reader: subscribe } = judgedTwice();
	// Reads every judged group above each run, and more past them: settled.
	const wider = subscribe([
		[1n, 2n],
		[4n, 8n],
	]);
	expect(hooks.tryDrop(wider)).toMatchObject({ start: 1n, end: 2n, stale: 500 });
	expect(hooks.tryDrop(wider)).toMatchObject({ start: 4n, end: 6n, stale: 500 });
	expect(hooks.tryDrop(wider)).toBeUndefined();
	expect(await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)])).toBeUndefined();
	// Stops at 4, which the upstream judged against 5: its own newest, which no verdict convicts.
	// Both runs are unknown to it, and asked for.
	const narrower = subscribe([
		[0n, 2n],
		[4n, 5n],
	]);
	expect(hooks.tryRecvGroup(narrower, false)).toEqual({ kind: "idle" });
	expect(hooks.tryDrop(narrower)).toBeUndefined();
	const asked: bigint[][] = [];
	for (;;) {
		const fill = await Promise.race([requests.requestedFill(), yieldNow().then(() => undefined)]);
		if (!fill) break;
		asked.push([fill.start, fill.end]);
	}
	asked.sort((a, b) => Number(a[0] - b[0]));
	expect(asked).toEqual([
		[0n, 2n],
		[4n, 5n],
	]);

	wider.close();
	narrower.close();
	requests.close();
	broadcast.close();
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
	if (!fill) throw new Error("no fill");
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
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI, maxAge: null });
	// Groups 0 to 2 were published before this producer existed.
	const live = track.createGroup(3n);
	live.writeFrame({ payload: enc.encode("live 3"), timestamp: Timestamp.fromMillis(3000) });
	live.finish();
	const requests = track.requests();
	const fills: Fill[] = [];
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			fills.push(fill);
			for (let sequence = fill.wanted(); sequence !== undefined; sequence = fill.wanted()) {
				if (sequence === 1n) {
					fill.dropGroups(1n, 2n);
					continue;
				}
				const group = fill.accept(sequence);
				group.writeFrame({
					payload: enc.encode(`old ${sequence}`),
					timestamp: Timestamp.fromMillis(Number(sequence) * 1000),
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
		const reader = peer.remote.track("video").subscribe({
			ranges: [{ start: { absolute: 0n }, end: { absolute: 4n } }],
			delay: { max: null },
			order: "ascending",
		});
		const got: [bigint, string[]][] = [];
		for (let group = await reader.recvGroup(); group; group = await reader.recvGroup()) {
			got.push([group.sequence, await frames(group)]);
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
	const track = broadcast.createTrack("video", { maxAge: Duration.millis(10_000) });
	const live = track.createGroup(3n);
	live.writeFrame({ payload: enc.encode("live 3") });
	live.finish();
	const requests = track.requests();
	const terms: Subscription = {
		ranges: [{ start: { absolute: 0n } }],
		delay: { max: Duration.millis(60_000) },
		order: "ascending",
	};
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
	expect(reader.tryRecvGroup()?.sequence).toBe(3n);
	reader.tryRecvGroup();
	// A reader of the run alone, which the verdict does not settle, waits on the same fill.
	const bounded = track.subscribe({ ...terms, ranges: [{ start: { absolute: 0n }, end: { absolute: 3n } }] });
	bounded.tryRecvGroup();
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	expect([fill.start, fill.end]).toEqual([0n, 3n]);
	const old = new StreamError(StreamCode.Old, { message: "old" });
	rejectGroups(fill, 0n, 3n, old, { budget: 10_000, everyone: true });
	expect(hooks.tryDrop(bounded)).toMatchObject({ kind: "drop", start: 0n, end: 3n, error: old });
	fill.close();

	const later = track.subscribe(terms);
	expect(later.tryRecvGroup()?.sequence).toBe(3n);
	later.tryRecvGroup();
	expect(hooks.tryDrop(later)).toMatchObject({ kind: "drop", start: 0n, end: 3n, stale: 10_000 });
	expect(await asked()).toEqual([]);

	expect(hooks.supersedeStale(track, undefined)).toEqual([{ start: 0n, end: 3n }]);
	const again = track.subscribe(terms);
	expect(again.tryRecvGroup()?.sequence).toBe(3n);
	again.tryRecvGroup();
	expect(await asked()).toEqual([[0n, 3n]]);

	reader.close();
	bounded.close();
	later.close();
	again.close();
	requests.close();
	broadcast.close();
});
