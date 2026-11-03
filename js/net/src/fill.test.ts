import { expect, test } from "bun:test";
import { Milli, yieldNow } from "@moq/clock";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import { accept as acceptSession, connect as connectSession } from "./connection/index.ts";
import { NotFound, StreamCode, StreamError, toStreamCode } from "./error.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
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
type Archive = Map<number, string[]>;

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
			fill.dropGroups(sequence, sequence + 1);
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
function resumed(first: number): { broadcast: BroadcastProducer; track: Producer } {
	const broadcast = new BroadcastProducer();
	const track = broadcast.createTrack("video");
	const group = track.createGroup(first);
	group.writeFrame({ payload: enc.encode(`live ${first}`) });
	group.finish();
	return { broadcast, track };
}

const history: Subscription = { ranges: [{ start: { absolute: 0 } }], delay: { max: null }, order: "ascending" };

test("fill: a fetch of a group never cached is served by the request handler, byte for byte", async () => {
	const { broadcast, track } = resumed(5);
	const requests = track.requests();
	const fills = respond(requests, new Map([[2, ["a", "b", "c"]]]));

	const group = await wireOf(broadcast).fetchGroup("video", 2);
	expect(group.sequence).toBe(2);
	expect(await frames(group)).toEqual(["a", "b", "c"]);
	expect(fills.map((fill) => [fill.start, fill.end])).toEqual([[2, 3]]);

	// Now cached: a second fetch asks nothing.
	const again = await wireOf(broadcast).fetchGroup("video", 2);
	expect(await frames(again)).toEqual(["a", "b", "c"]);
	expect(fills.length).toBe(1);

	requests.close();
	broadcast.close();
});

test("fill: an empty group replays as empty, and a missing one fails the fetch with NotFound", async () => {
	const { broadcast, track } = resumed(5);
	const requests = track.requests();
	respond(requests, new Map([[1, []]]));

	const empty = await wireOf(broadcast).fetchGroup("video", 1);
	expect(await frames(empty)).toEqual([]);
	await expect(wireOf(broadcast).fetchGroup("video", 3)).rejects.toBeInstanceOf(NotFound);

	requests.close();
	broadcast.close();
});

test("fill: without a handler, a fetch of an uncached group fails at once", async () => {
	const { broadcast } = resumed(5);
	await expect(wireOf(broadcast).fetchGroup("video", 2)).rejects.toBeInstanceOf(NotFound);
	broadcast.close();
});

test("fill: concurrent fetches of one group share one fill", async () => {
	const { broadcast, track } = resumed(5);
	const requests = track.requests();
	const a = wireOf(broadcast).fetchGroup("video", 3);
	const b = wireOf(broadcast).fetchGroup("video", 3);

	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	expect([fill.start, fill.end]).toEqual([3, 4]);
	// The second fetch joined it rather than queueing its own.
	await yieldNow();
	const queued = await Promise.race([requests.requestedFill(), yieldNow().then(() => "none")]);
	expect(queued).toBe("none");

	serve(fill, new Map([[3, ["x"]]]));
	const [first, second] = await Promise.all([a, b]);
	expect(await frames(first)).toEqual(["x"]);
	expect(await frames(second)).toEqual(["x"]);

	requests.close();
	broadcast.close();
});

test("fill: a fetch cancelled while its fill awaits a reply withdraws the fill", async () => {
	const { broadcast, track } = resumed(5);
	const requests = track.requests();
	const controller = new AbortController();
	const fetching = wireOf(broadcast).fetchGroup("video", 2, { signal: controller.signal });

	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	expect(await fill.wantedChanged()).toBe(2);
	expect(fill.isWanted(2)).toBe(true);

	const reason = new Error("left");
	controller.abort(reason);
	await expect(fetching).rejects.toBe(reason);
	expect(await fill.wantedChanged()).toBeUndefined();
	expect(fill.isWanted(2)).toBe(false);
	// Nothing reads it, so closing the withdrawn fill records nothing.
	fill.close();
	const later = wireOf(broadcast).fetchGroup("video", 2);
	const refill = await requests.requestedFill();
	expect(refill?.start).toBe(2);
	if (refill) serve(refill, new Map([[2, ["late"]]]));
	expect(await frames(await later)).toEqual(["late"]);

	requests.close();
	broadcast.close();
});

test("fill: a reader reaching back for history gets the misses filled in order", async () => {
	const { broadcast, track } = resumed(4);
	const requests = track.requests();
	const fills = respond(
		requests,
		new Map([
			[0, ["g0"]],
			[1, ["g1"]],
			[3, ["g3"]],
		]),
	);

	const reader = track.subscribe(history);
	const ordered = reader.ordered();
	const seen: [number, string[]][] = [];
	for (let i = 0; i < 4; i++) {
		const group = await ordered.nextGroup();
		if (!group) break;
		seen.push([group.sequence, await frames(group)]);
	}
	// Group 2 is missing from the archive, so it is dropped and the cursor moves past it.
	expect(seen).toEqual([
		[0, ["g0"]],
		[1, ["g1"]],
		[3, ["g3"]],
		[4, ["live 4"]],
	]);
	expect(fills.map((fill) => [fill.start, fill.end])).toEqual([[0, 4]]);

	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: a bounded reader ends once its fill answers every group", async () => {
	const { broadcast, track } = resumed(4);
	const requests = track.requests();
	respond(requests, new Map([[1, ["g1"]]]));

	const reader = track.subscribe({
		ranges: [{ start: { absolute: 0 }, end: { absolute: 3 } }],
		delay: { max: null },
	});
	const got: number[] = [];
	for (let group = await reader.recvGroup(); group; group = await reader.recvGroup()) got.push(group.sequence);
	expect(got).toEqual([1]);

	requests.close();
	broadcast.close();
});

test("fill: a reader at the live edge asks nothing", async () => {
	const { broadcast, track } = resumed(4);
	const requests = track.requests();
	const reader = track.subscribe();
	expect((await reader.recvGroup())?.sequence).toBe(4);
	const queued = await Promise.race([requests.requestedFill(), yieldNow().then(() => "none")]);
	expect(queued).toBe("none");

	reader.close();
	requests.close();
	broadcast.close();
});

test("fill: closing a fill drops what is still wanted, and closing the handler drops what is queued", async () => {
	const { broadcast, track } = resumed(5);
	const requests = track.requests();
	const a = wireOf(broadcast).fetchGroup("video", 1);
	const fill = await requests.requestedFill();
	fill?.close();
	await expect(a).rejects.toBeInstanceOf(NotFound);

	const b = wireOf(broadcast).fetchGroup("video", 3);
	await yieldNow();
	requests.close();
	await expect(b).rejects.toBeInstanceOf(NotFound);
	// The last handler left, so a miss fails at once again.
	await expect(wireOf(broadcast).fetchGroup("video", 2)).rejects.toBeInstanceOf(NotFound);

	broadcast.close();
});

test("fill: a refusal fails the readers waiting now, and a later reader asks again", async () => {
	const { broadcast, track } = resumed(5);
	const requests = track.requests();
	const waiting = wireOf(broadcast).fetchGroup("video", 2);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	const refusal = new StreamError(StreamCode.Cancel, { message: "storage busy" });
	fill.refuseGroups(2, 3, refusal);
	fill.close();
	await expect(waiting).rejects.toBe(refusal);

	const later = wireOf(broadcast).fetchGroup("video", 2);
	const again = await requests.requestedFill();
	expect(again?.start).toBe(2);
	if (again) serve(again, new Map([[2, ["ok"]]]));
	expect(await frames(await later)).toEqual(["ok"]);

	requests.close();
	broadcast.close();
});

test("fill: a fill names the most urgent priority and loosest budget among its readers", async () => {
	const { broadcast, track } = resumed(5);
	const requests = track.requests();
	const low = track.subscribe({ ...history, priority: 1, delay: { max: Milli(60_000) } });
	void drain(low);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	expect([fill.start, fill.end]).toEqual([0, 5]);
	expect(fill.priority).toBe(1);
	expect(fill.maxDelay).toBe(Milli(60_000));
	expect(fill.order).toBe("ascending");

	const high = track.subscribe({ ...history, priority: 7, order: "descending" });
	void drain(high);
	await yieldNow();
	expect(fill.priority).toBe(7);
	expect(fill.maxDelay).toBeNull();
	expect(fill.order).toBe("descending");
	expect(fill.wanted()).toBe(4);

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
	const late = track.createGroup(3);
	late.finish();
	const fetching = wireOf(broadcast).fetchGroup("video", 1);
	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");

	// The producer writes it itself before the handler gets to it.
	const live = track.createGroup(1);
	live.writeFrame({ payload: enc.encode("live") });
	live.finish();
	expect(fill.wanted()).toBeUndefined();
	expect(() => fill.accept(1)).toThrow();
	expect(await frames(await fetching)).toEqual(["live"]);

	fill.close();
	requests.close();
	broadcast.close();
});

test("fill: a cache-miss fetch of a dynamic track serves the one track the broadcast already has", async () => {
	const broadcast = new BroadcastProducer();
	// Pulled first, so the broadcast serves tracks on demand.
	const pulled = wireOf(broadcast).requested();
	const fetching = wireOf(broadcast).fetchGroup("video", 1);
	const request = await pulled;
	if (!request) throw new Error("no track request");
	const track = request.accept();
	const requests = track.requests();
	const group = track.createGroup(4);
	group.finish();

	const fill = await requests.requestedFill();
	if (!fill) throw new Error("no fill");
	serve(fill, new Map([[1, ["one"]]]));
	expect(await frames(await fetching)).toEqual(["one"]);

	// A second miss and a live reader land on the same producer, not a second request.
	const second = wireOf(broadcast).fetchGroup("video", 2);
	const queued = await Promise.race([wireOf(broadcast).requested(), yieldNow().then(() => "none")]);
	expect(queued).toBe("none");
	const refill = await requests.requestedFill();
	if (refill) serve(refill, new Map([[2, ["two"]]]));
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
	const publication = origin.createBroadcast(Path.from("archive"));
	publication.announce();
	const broadcast = publication.broadcast;
	const track = broadcast.createTrack("video", { timescale: Timescale.MILLI });
	// Groups 0 to 2 were published before this producer existed.
	const live = track.createGroup(3);
	live.writeFrame({ payload: enc.encode("live 3"), timestamp: Timestamp.fromMillis(3000) });
	live.finish();
	const requests = track.requests();
	const fills: Fill[] = [];
	void (async () => {
		for (let fill = await requests.requestedFill(); fill; fill = await requests.requestedFill()) {
			fills.push(fill);
			for (let sequence = fill.wanted(); sequence !== undefined; sequence = fill.wanted()) {
				if (sequence === 1) {
					fill.dropGroups(1, 2);
					continue;
				}
				const group = fill.accept(sequence);
				group.writeFrame({
					payload: enc.encode(`old ${sequence}`),
					timestamp: Timestamp.fromMillis(sequence * 1000),
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
			const group = await peer.remote.track("video").fetchGroup(2);
			const frame = await group.readFrame();
			expect(dec.decode(frame?.payload)).toBe("old 2");
			expect(frame?.timestamp?.asMillis()).toBe(2000);
			expect(await group.readFrame()).toBeUndefined();

			const missing = await peer.remote
				.track("video")
				.fetchGroup(1)
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
			ranges: [{ start: { absolute: 0 }, end: { absolute: 4 } }],
			delay: { max: null },
			order: "ascending",
		});
		const got: [number, string[]][] = [];
		for (let group = await reader.recvGroup(); group; group = await reader.recvGroup()) {
			got.push([group.sequence, await frames(group)]);
		}
		got.sort(([a], [b]) => a - b);
		expect(got).toEqual([
			[0, ["old 0"]],
			[2, ["old 2"]],
			[3, ["live 3"]],
		]);
		reader.close();
	} finally {
		peer.close();
	}
});
