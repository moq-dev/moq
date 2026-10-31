import { expect, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import { NotFound, StreamCode, StreamError } from "./error.ts";
import type { Tracks } from "./listing.ts";
import { wireOf } from "./wire.ts";

/** Lets queued microtasks and the listing's signal notifications run. */
function settle(): Promise<void> {
	return yieldNow();
}

/**
 * Reads a listing as `+name`, `-name`, `synced`, `end`, or `error: ..`, keeping one pending read
 * across calls so a read that loses to {@link drain}'s settle never drops an event.
 */
class Events {
	readonly tracks: Tracks;
	#pending?: Promise<string>;

	constructor(tracks: Tracks) {
		this.tracks = tracks;
	}

	/** Every event ready now, through the end of the listing. */
	async drain(): Promise<string[]> {
		const events: string[] = [];
		for (;;) {
			this.#pending ??= this.tracks.next().then(
				(event) => {
					if (!event) return "end";
					if (event.kind === "synced") return "synced";
					const sign = { added: "+", removed: "-", finished: "~" }[event.kind];
					return `${sign}${event.name}`;
				},
				(err: unknown) => `error: ${err instanceof StreamError ? err.code : String(err)}`,
			);
			const event = await Promise.race([this.#pending, settle().then(() => undefined)]);
			if (event === undefined) return events;
			this.#pending = undefined;
			events.push(event);
			if (event === "end" || event.startsWith("error")) return events;
		}
	}
}

function list(broadcast: BroadcastProducer): Events {
	return new Events(broadcast.consume().tracks());
}

test("tracks present arrive before synced", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("a");
	broadcast.createTrack("b");
	expect(await list(broadcast).drain()).toEqual(["+a", "+b", "synced"]);
});

test("an empty listing syncs at once", async () => {
	const broadcast = new BroadcastProducer();
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["synced"]);
	broadcast.createTrack("a");
	expect(await tracks.drain()).toEqual(["+a"]);
});

test("a listed track reports its info", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("a", { priority: 9, refresh: 4 });
	const event = await broadcast.consume().tracks().next();
	expect(event?.kind === "added" && event.info).toMatchObject({ priority: 9, refresh: 4 });
});

test("a track leaving the table is removed", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("a");
	const b = broadcast.createTrack("b");
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+a", "+b", "synced"]);
	broadcast.removeTrack("a");
	b.abort(new Error("aborted"));
	expect(await tracks.drain()).toEqual(["-a", "-b"]);
});

test("a finished track is listed while the table holds it", async () => {
	const broadcast = new BroadcastProducer();
	const a = broadcast.createTrack("a");
	a.finish();
	a.close();
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+a", "synced"]);
	// It finished before it was removed.
	broadcast.removeTrack("a");
	expect(await tracks.drain()).toEqual(["~a"]);
	broadcast.close();
	expect(await tracks.drain()).toEqual(["end"]);
});

test("a track that finished but stays open ends the listing as finished", async () => {
	const broadcast = new BroadcastProducer();
	const a = broadcast.createTrack("a");
	broadcast.createTrack("b");
	a.finish();
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+a", "+b", "synced"]);
	broadcast.close();
	expect(await tracks.drain()).toEqual(["~a", "end"]);
});

test("a lister finishes or removes a track", async () => {
	const broadcast = new BroadcastProducer();
	const lister = wireOf(broadcast).lister();
	const tracks = list(broadcast);
	lister.add("up", { priority: 0 });
	lister.add("gone", { priority: 0 });
	lister.synced();
	expect(await tracks.drain()).toEqual(["+up", "+gone", "synced"]);
	lister.finish("up");
	lister.remove("gone");
	expect(await tracks.drain()).toEqual(["~up", "-gone"]);
	expect(() => lister.finish("up")).toThrow();
});

test("a recreated track leaves as it ended before it is added again", async () => {
	const broadcast = new BroadcastProducer();
	const a = broadcast.createTrack("a");
	a.finish();
	a.close();
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+a", "synced"]);
	// The finished track still holds the name; its replacement takes it over.
	broadcast.createTrack("a");
	expect(await tracks.drain()).toEqual(["~a", "+a"]);
});

test("a track served on request is never listed", async () => {
	const broadcast = new BroadcastProducer();
	const consumer = broadcast.consume();
	const tracks = new Events(consumer.tracks());
	const pulled = wireOf(broadcast).requested();
	expect(await tracks.drain()).toEqual(["synced"]);

	consumer.track("x").subscribe();
	const request = await pulled;
	request?.accept({});
	expect(await tracks.drain()).toEqual([]);

	// A queued request answered by createTrack is the producer's track, so it is listed.
	consumer.track("z").subscribe();
	broadcast.createTrack("z");
	expect(await tracks.drain()).toEqual(["+z"]);
	broadcast.close();
});

test("an initial track removed before it is read is omitted", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("a");
	broadcast.createTrack("b");
	const tracks = list(broadcast);
	broadcast.removeTrack("a");
	expect(await tracks.drain()).toEqual(["+b", "synced"]);
});

test("churn never postpones synced", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("a");
	broadcast.createTrack("b");
	const tracks = broadcast.consume().tracks();
	// New names keep arriving faster than the reader drains them.
	broadcast.createTrack("c");
	expect(await tracks.next()).toMatchObject({ kind: "added", name: "a" });
	broadcast.createTrack("d");
	expect(await tracks.next()).toMatchObject({ kind: "added", name: "b" });
	broadcast.createTrack("e");
	expect(await new Events(tracks).drain()).toEqual(["synced", "+c", "+d", "+e"]);
});

test("a removal of a seen track may precede synced", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("a");
	broadcast.createTrack("b");
	broadcast.createTrack("c");
	const tracks = broadcast.consume().tracks();
	expect(await tracks.next()).toMatchObject({ kind: "added", name: "a" });
	broadcast.removeTrack("a");
	broadcast.removeTrack("c");
	expect(await new Events(tracks).drain()).toEqual(["+b", "-a", "synced"]);
});

test("a stalled reader holds one change per name", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("seen");
	broadcast.createTrack("stay");
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+seen", "+stay", "synced"]);
	broadcast.removeTrack("seen");
	for (let round = 0; round < 1000; round++) {
		broadcast.createTrack(`churn${round}`);
		broadcast.createTrack("seen");
		broadcast.removeTrack(`churn${round}`);
		broadcast.removeTrack("seen");
	}
	expect(await tracks.drain()).toEqual(["-seen"]);
});

test("the broadcast ending ends the listing", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("a");
	const consumer = broadcast.consume();
	const tracks = new Events(consumer.tracks());
	broadcast.close();
	expect(await tracks.drain()).toEqual(["end"]);
	expect(() => consumer.tracks()).toThrow("broadcast is closed");
});

// A clean end reports each track that left before it, and how, but not one the reader never
// saw; an error ends at once.
test("a clean end follows the removals before it", async () => {
	const broadcast = new BroadcastProducer();
	const a = broadcast.createTrack("a");
	broadcast.createTrack("b");
	const live = broadcast.createTrack("live");
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+a", "+b", "+live", "synced"]);
	a.finish();
	a.close();
	broadcast.removeTrack("a");
	broadcast.removeTrack("b");
	broadcast.createTrack("unseen");
	live.finish();
	live.close();
	broadcast.close();
	expect(await tracks.drain()).toEqual(["~a", "-b", "~live", "end"]);

	// The producer's finished track is what a handler's answer of its name served.
	const both = new BroadcastProducer();
	const handler = wireOf(both).lister();
	handler.add("a", { priority: 0 });
	handler.synced();
	const own = both.createTrack("a");
	const served = list(both);
	expect(await served.drain()).toEqual(["+a", "synced"]);
	own.finish();
	own.close();
	both.close();
	expect(await served.drain()).toEqual(["~a", "end"]);

	// A reader that closes itself reads nothing more, even what was queued before the end.
	const closing = new BroadcastProducer();
	const gone = closing.createTrack("gone");
	const reader = list(closing);
	expect(await reader.drain()).toEqual(["+gone", "synced"]);
	gone.finish();
	gone.close();
	closing.removeTrack("gone");
	closing.close();
	reader.tracks.close();
	expect(await reader.drain()).toEqual(["end"]);

	for (const abort of [false, true]) {
		const answered = new BroadcastProducer();
		const lister = wireOf(answered).lister();
		const reader = list(answered);
		lister.add("up", { priority: 0 });
		lister.synced();
		expect(await reader.drain()).toEqual(["+up", "synced"]);
		lister.finish("up");
		if (abort) {
			lister.abort(new NotFound("broadcast"));
			expect(await reader.drain()).toEqual([`error: ${StreamCode.NotFound}`]);
		} else {
			lister.end();
			expect(await reader.drain()).toEqual(["~up", "end"]);
		}
	}
});

test("a closed listing reads nothing more, and leaving a loop early closes it", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("a");
	const tracks = broadcast.consume().tracks();
	for await (const event of tracks) {
		expect(event).toMatchObject({ kind: "added", name: "a" });
		break;
	}
	broadcast.createTrack("b");
	expect(await tracks.next()).toBeUndefined();
});

test("readers wait for the lister to sync", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("own");
	const lister = wireOf(broadcast).lister();
	expect(lister.wanted.peek()).toBe(false);

	const tracks = list(broadcast);
	expect(lister.wanted.peek()).toBe(true);
	expect(await tracks.drain()).toEqual(["+own"]);
	lister.add("up", { priority: 0 });
	expect(() => lister.add("up", { priority: 0 })).toThrow("duplicate");
	lister.synced();
	expect(await tracks.drain()).toEqual(["+up", "synced"]);

	lister.remove("up");
	expect(() => lister.remove("up")).toThrow("not listed");
	expect(await tracks.drain()).toEqual(["-up"]);

	tracks.tracks.close();
	expect(lister.wanted.peek()).toBe(false);
});

test("a name listed by both is reported once", async () => {
	const broadcast = new BroadcastProducer();
	broadcast.createTrack("a");
	const lister = wireOf(broadcast).lister();
	lister.add("a", { priority: 0 });
	lister.synced();
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+a", "synced"]);
	broadcast.removeTrack("a");
	expect(await tracks.drain()).toEqual([]);
	lister.remove("a");
	expect(await tracks.drain()).toEqual(["-a"]);
});

test("an aborted answer fails its readers and a later reader asks again", async () => {
	const broadcast = new BroadcastProducer();
	const lister = wireOf(broadcast).lister();
	const tracks = list(broadcast);
	lister.add("up", { priority: 0 });
	lister.abort(new NotFound("broadcast"));
	expect(await tracks.drain()).toEqual([`error: ${StreamCode.NotFound}`]);
	expect(lister.wanted.peek()).toBe(false);

	const again = list(broadcast);
	expect(lister.wanted.peek()).toBe(true);
	expect(await again.drain()).toEqual([]);
	lister.synced();
	expect(await again.drain()).toEqual(["synced"]);
	// The failed reader leaving releases nothing.
	tracks.tracks.close();
	expect(lister.wanted.peek()).toBe(true);
});

test("a closed lister withdraws its answer and syncs", async () => {
	const broadcast = new BroadcastProducer();
	const lister = wireOf(broadcast).lister();
	lister.add("up", { priority: 0 });
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+up"]);
	lister.close();
	expect(await tracks.drain()).toEqual(["-up", "synced"]);
});

test("an owner lists each broadcast once", async () => {
	const broadcast = new BroadcastProducer();
	const other = new BroadcastProducer();
	broadcast.createTrack("a");
	const owner = {};
	const older = new Events(wireOf(broadcast).tracks(owner));
	const elsewhere = new Events(wireOf(other).tracks(owner));
	const stranger = new Events(wireOf(broadcast).tracks({}));
	const newer = new Events(wireOf(broadcast).tracks(owner));
	expect(await older.drain()).toEqual([`error: ${StreamCode.Cancel}`]);
	expect(await newer.drain()).toEqual(["+a", "synced"]);
	expect(await elsewhere.drain()).toEqual(["synced"]);
	expect(await stranger.drain()).toEqual(["+a", "synced"]);

	// The replaced reader leaving releases nothing of the newer one's.
	older.tracks.close();
	broadcast.createTrack("b");
	expect(await newer.drain()).toEqual(["+b"]);
	newer.tracks.close();
	stranger.tracks.close();
	expect(wireOf(broadcast).lister().wanted.peek()).toBe(false);
});

test("a broadcast has one lister", () => {
	const broadcast = new BroadcastProducer();
	const lister = wireOf(broadcast).lister();
	expect(() => wireOf(broadcast).lister()).toThrow("already has a lister");
	lister.close();
	wireOf(broadcast).lister();
});

test("an ended answer ends its readers and a later reader asks again", async () => {
	const broadcast = new BroadcastProducer();
	const lister = wireOf(broadcast).lister();
	const tracks = list(broadcast);
	lister.add("up", { priority: 0 });
	lister.synced();
	expect(await tracks.drain()).toEqual(["+up", "synced"]);
	lister.end();
	expect(await tracks.drain()).toEqual(["end"]);
	expect(lister.wanted.peek()).toBe(false);

	const again = list(broadcast);
	expect(await again.drain()).toEqual([]);
});

test("a cleared answer unsyncs a reader still waiting", async () => {
	const broadcast = new BroadcastProducer();
	const lister = wireOf(broadcast).lister();
	broadcast.createTrack("a");
	broadcast.createTrack("b");
	lister.synced();
	const tracks = broadcast.consume().tracks();
	expect(await tracks.next()).toMatchObject({ kind: "added", name: "a" });
	lister.clear();
	const events = new Events(tracks);
	expect(await events.drain()).toEqual(["+b"]);
	lister.synced();
	expect(await events.drain()).toEqual(["synced"]);
});

test("a lister sees the broadcast end", async () => {
	const broadcast = new BroadcastProducer();
	const lister = wireOf(broadcast).lister();
	broadcast.close();
	expect(lister.closed.peek()).toBeNull();
});
