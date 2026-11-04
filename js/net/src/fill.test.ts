import { expect, test } from "bun:test";
import { Manual, Milli, Nano, yieldNow } from "@moq/clock";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import { accept as acceptSession, connect as connectSession } from "./connection/index.ts";
import { NotFound, StreamCode, StreamError, toStreamCode } from "./error.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import { hooks } from "./internal.ts";
import * as Lite from "./lite/index.ts";
import { createMockTransportPair } from "./mock.ts";
import { Producer as OriginProducer } from "./origin.ts";
import * as Path from "./path.ts";
import { Timescale, Timestamp } from "./time.ts";
import type { Fill, Producer, Requests, Subscription } from "./track.ts";
import { wireOf } from "./wire.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

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
	const track = broadcast.createTrack("video");
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

test("fill: a fill names the most urgent priority and loosest budget among its readers", async () => {
	const { broadcast, track } = resumed(5n);
	const requests = track.requests();
	const low = track.subscribe({ ...history, priority: 1, delay: { max: Milli(60_000) } });
	void drain(low);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	expect([fill.start, fill.end]).toEqual([0n, 5n]);
	expect(fill.priority).toBe(1);
	expect(fill.maxDelay).toBe(Milli(60_000));
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

test("fill: a group arriving live answers the fill", async () => {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
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
			delay: { max: Milli(1000) },
			order: "ascending",
		});
		const next = reader.ordered().nextGroup();
		// The handler takes the fill for the missing groups and never answers.
		const fill = await requests.requestedFill();
		expect([fill?.start, fill?.end]).toEqual([0n, 5n]);

		// Group 5 arrived when the reader asked, so its budget gives up the holes below a second on.
		await clock.advance(Nano.fromMilli(Milli(1001)));
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
		const reader = track.subscribe({ ranges: [{ start: { absolute: 0n } }], delay: { max: Milli(1000) } });
		expect((await reader.recvGroup())?.sequence).toBe(5n);
		const next = reader.recvGroup();
		const fill = await requests.requestedFill();
		expect([fill?.start, fill?.end]).toEqual([0n, 5n]);

		// The fill never answers; the holes go past the budget, and the finished track ends the read.
		await clock.advance(Nano.fromMilli(Milli(1001)));
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
			delay: { max: Milli(1000) },
			order: "ascending",
		});
		const next = reader.ordered().nextGroup();
		const fill = await requests.requestedFill();
		if (!fill) throw new Error("no fill");
		expect(await fill.wantedChanged()).toBe(0n);
		const changed = fill.wantedChanged();
		const unwanted = fill.unwanted(0n);

		// Past the budget the reader takes group 5, leaving the holes below it for good.
		await clock.advance(Nano.fromMilli(Milli(1001)));
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
		const track = broadcast.createTrack("video");
		const write = (sequence: bigint) => {
			const group = track.createGroup(sequence);
			group.writeFrame({ payload: enc.encode(`live ${sequence}`) });
			group.finish();
		};
		// Holes 0-1 wait on group 2, which arrives first; holes 3-4 on group 5, half a second later.
		write(2n);
		await clock.advance(Nano.fromMilli(Milli(500)));
		write(5n);
		const requests = track.requests();
		const reader = track.subscribe({
			ranges: [{ start: { absolute: 0n }, end: { absolute: 6n } }],
			delay: { max: Milli(1000) },
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
		await clock.advance(Nano.fromMilli(Milli(600)));
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

test("fill: a sequence the track's datagram took is never asked, even once a reader read it", async () => {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	// Subscribed first: a datagram is not replayed to a later reader.
	const reader = track.subscribe(history);
	track.insertDatagram(2n, undefined, enc.encode("datagram 2"));
	const group = track.createGroup(5n);
	group.writeFrame({ payload: enc.encode("live 5") });
	group.finish();
	// The reader takes the datagram out of its own buffer; the track still holds it.
	expect((await reader.recvDatagram())?.sequence).toBe(2n);

	const requests = track.requests();
	const ordered = reader.ordered();
	void ordered.nextGroup();
	const fills = [await requests.requestedFill(), await requests.requestedFill()];
	expect(fills.map((fill) => [fill?.start, fill?.end])).toEqual([
		[0n, 2n],
		[3n, 5n],
	]);

	for (const fill of fills) fill?.close();
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
	hooks.dropTrackGroups(track, 2n, 3n, cut);
	expect(fill.wanted()).toBeUndefined();
	await expect(fetching).rejects.toBe(cut);

	fill.close();
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
	const track = request.accept();
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
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
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
