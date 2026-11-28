import { expect, test } from "bun:test";
import { yieldNow } from "@moq/clock";
import { Producer as BroadcastProducer } from "./broadcast.ts";
import { defined } from "./defined.test-util.ts";
import { NotFound, StreamCode, StreamError } from "./error.ts";
import type { Tracks } from "./listing.ts";
import type { Info } from "./track.ts";
import { wireOf } from "./wire.ts";

const INFO: Info = {
	compression: "none",
	timescale: null,
	maxAge: null,
	priority: 0,
	refresh: { ratio: null, interval: null },
};

/** Lets queued microtasks and the listing's signal notifications run. */
function settle(): Promise<void> {
	return yieldNow();
}

/**
 * Reads a listing as `+name`, `-name`, `~name` (finished), `synced`, `end`, or `error: ..`, a
 * datagram track's name prefixed `d:`, keeping one pending read across calls so a read that loses
 * to {@link drain}'s settle never drops an event.
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
					return `${sign}${event.datagrams ? "d:" : ""}${event.name}`;
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
	defined(broadcast.createTrack("a"));
	defined(broadcast.createTrack("b"));
	expect(await list(broadcast).drain()).toEqual(["+a", "+b", "synced"]);
});

test("an empty listing syncs at once", async () => {
	const broadcast = new BroadcastProducer();
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["synced"]);
	defined(broadcast.createTrack("a"));
	expect(await tracks.drain()).toEqual(["+a"]);
});

test("a listed track reports its info", async () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("a", { priority: 9, refresh: { ratio: 4 } }));
	const event = await broadcast.consume().tracks().next();
	expect(event?.kind === "added" && event.info).toMatchObject({ priority: 9, refresh: { ratio: 4, interval: null } });
});

test("a datagram track is listed until it ends", async () => {
	const broadcast = new BroadcastProducer();
	const early = defined(broadcast.createDatagrams("early", { priority: 4 }));
	const event = await broadcast.consume().tracks().next();
	expect(event).toMatchObject({ kind: "added", name: "early", datagrams: true, info: { priority: 4 } });

	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+d:early", "synced"]);
	const late = defined(broadcast.createDatagrams("late"));
	expect(await tracks.drain()).toEqual(["+d:late"]);
	early.finish();
	late.abort(new Error("aborted"));
	expect(await tracks.drain()).toEqual(["~d:early", "-d:late"]);

	// A name ended is free for a new track, listed afresh.
	defined(broadcast.createDatagrams("early"));
	expect(await tracks.drain()).toEqual(["+d:early"]);
});

test("the two kinds of a name are listed apart", async () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("a"));
	const datagrams = defined(broadcast.createDatagrams("a"));
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+a", "+d:a", "synced"]);
	broadcast.removeTrack("a");
	expect(await tracks.drain()).toEqual(["-a"]);
	datagrams.finish();
	expect(await tracks.drain()).toEqual(["~d:a"]);

	const lister = wireOf(broadcast).lister();
	lister.add("b", { info: INFO });
	lister.add("b", { datagrams: true, info: { priority: 0, timescale: null } });
	expect(() => lister.add("b", { datagrams: true, info: { priority: 0, timescale: null } })).toThrow("duplicate");
	lister.synced();
	expect(await tracks.drain()).toEqual(["+b", "+d:b"]);
	lister.finish("b", true);
	expect(() => lister.remove("b", true)).toThrow("not listed");
	lister.remove("b");
	expect(await tracks.drain()).toEqual(["~d:b", "-b"]);
});

test("a datagram track served on request is never listed", async () => {
	const broadcast = new BroadcastProducer();
	const consumer = broadcast.consume();
	const requested = wireOf(broadcast).requestedDatagrams();
	const tracks = new Events(consumer.tracks());
	expect(await tracks.drain()).toEqual(["synced"]);
	consumer.datagrams("x");
	defined((await requested)?.accept({}));
	// A session's datagram track riding a stream track's subscription is not listed either.
	defined(wireOf(broadcast).createDatagrams("y", {}, false));
	expect(await tracks.drain()).toEqual([]);
});

test("a track leaving the table is removed", async () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("a"));
	const b = defined(broadcast.createTrack("b"));
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+a", "+b", "synced"]);
	broadcast.removeTrack("a");
	b.abort(new Error("aborted"));
	expect(await tracks.drain()).toEqual(["-a", "-b"]);
});

test("a finished track is listed while the table holds it", async () => {
	const broadcast = new BroadcastProducer();
	const a = defined(broadcast.createTrack("a"));
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
	const a = defined(broadcast.createTrack("a"));
	defined(broadcast.createTrack("b"));
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
	lister.add("up", { info: INFO });
	lister.add("gone", { info: INFO });
	lister.synced();
	expect(await tracks.drain()).toEqual(["+up", "+gone", "synced"]);
	lister.finish("up");
	lister.remove("gone");
	expect(await tracks.drain()).toEqual(["~up", "-gone"]);
	expect(() => lister.finish("up")).toThrow();
});

test("a recreated track leaves as it ended before it is added again", async () => {
	const broadcast = new BroadcastProducer();
	const a = defined(broadcast.createTrack("a"));
	a.finish();
	a.close();
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+a", "synced"]);
	// The finished track still holds the name; its replacement takes it over.
	defined(broadcast.createTrack("a"));
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
	defined(broadcast.createTrack("z"));
	expect(await tracks.drain()).toEqual(["+z"]);
	broadcast.close();
});

test("an initial track removed before it is read is omitted", async () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("a"));
	defined(broadcast.createTrack("b"));
	const tracks = list(broadcast);
	broadcast.removeTrack("a");
	expect(await tracks.drain()).toEqual(["+b", "synced"]);
});

test("churn never postpones synced", async () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("a"));
	defined(broadcast.createTrack("b"));
	const tracks = broadcast.consume().tracks();
	// New names keep arriving faster than the reader drains them.
	defined(broadcast.createTrack("c"));
	expect(await tracks.next()).toMatchObject({ kind: "added", name: "a" });
	defined(broadcast.createTrack("d"));
	expect(await tracks.next()).toMatchObject({ kind: "added", name: "b" });
	defined(broadcast.createTrack("e"));
	expect(await new Events(tracks).drain()).toEqual(["synced", "+c", "+d", "+e"]);
});

test("a cancelled track owes synced nothing, and a later change follows it", async () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("a"));
	defined(broadcast.createTrack("b"));
	defined(broadcast.createTrack("c"));
	const tracks = broadcast.consume().tracks();
	expect(await tracks.next()).toMatchObject({ kind: "added", name: "a" });
	broadcast.removeTrack("a");
	broadcast.removeTrack("c");
	expect(await new Events(tracks).drain()).toEqual(["+b", "synced", "-a"]);
});

test("a stalled reader holds one change per name", async () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("seen"));
	defined(broadcast.createTrack("stay"));
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+seen", "+stay", "synced"]);
	broadcast.removeTrack("seen");
	for (let round = 0; round < 1000; round++) {
		defined(broadcast.createTrack(`churn${round}`));
		defined(broadcast.createTrack("seen"));
		broadcast.removeTrack(`churn${round}`);
		broadcast.removeTrack("seen");
	}
	expect(await tracks.drain()).toEqual(["-seen"]);
});

test("the broadcast ending ends the listing", async () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("a"));
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
	const a = defined(broadcast.createTrack("a"));
	defined(broadcast.createTrack("b"));
	const live = defined(broadcast.createTrack("live"));
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+a", "+b", "+live", "synced"]);
	a.finish();
	a.close();
	broadcast.removeTrack("a");
	broadcast.removeTrack("b");
	defined(broadcast.createTrack("unseen"));
	live.finish();
	live.close();
	broadcast.close();
	expect(await tracks.drain()).toEqual(["~a", "-b", "~live", "end"]);

	// The producer's finished track is what a handler's answer of its name served.
	const both = new BroadcastProducer();
	const handler = wireOf(both).lister();
	handler.add("a", { info: INFO });
	handler.synced();
	const own = defined(both.createTrack("a"));
	const served = list(both);
	expect(await served.drain()).toEqual(["+a", "synced"]);
	own.finish();
	own.close();
	both.close();
	expect(await served.drain()).toEqual(["~a", "end"]);

	// A reader that closes itself reads nothing more, even what was queued before the end.
	const closing = new BroadcastProducer();
	const gone = defined(closing.createTrack("gone"));
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
		lister.add("up", { info: INFO });
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
	defined(broadcast.createTrack("a"));
	const tracks = broadcast.consume().tracks();
	for await (const event of tracks) {
		expect(event).toMatchObject({ kind: "added", name: "a" });
		break;
	}
	defined(broadcast.createTrack("b"));
	expect(await tracks.next()).toBeUndefined();
});

test("readers wait for the lister to sync", async () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("own"));
	const lister = wireOf(broadcast).lister();
	expect(lister.wanted.peek()).toBe(false);

	const tracks = list(broadcast);
	expect(lister.wanted.peek()).toBe(true);
	expect(await tracks.drain()).toEqual(["+own"]);
	lister.add("up", { info: INFO });
	expect(() => lister.add("up", { info: INFO })).toThrow("duplicate");
	lister.synced();
	expect(await tracks.drain()).toEqual(["+up", "synced"]);

	lister.remove("up");
	expect(() => lister.remove("up")).toThrow("not listed");
	expect(await tracks.drain()).toEqual(["-up"]);

	tracks.tracks.close();
	expect(lister.wanted.peek()).toBe(false);
});

test("a removal re-added after sync keeps synced behind the changes queued before it", async () => {
	const broadcast = new BroadcastProducer();
	const lister = wireOf(broadcast).lister();
	const tracks = list(broadcast);
	lister.add("a", { info: INFO });
	expect(await tracks.drain()).toEqual(["+a"]);
	lister.remove("a");
	lister.add("b", { info: INFO });
	lister.synced();
	// Churn coalesces in place, so it owes synced nothing extra.
	for (let round = 0; round < 3; round++) {
		lister.add("a", { info: INFO });
		lister.remove("a");
	}
	lister.add("a", { info: INFO });
	expect(await tracks.drain()).toEqual(["-a", "+a", "+b", "synced"]);
});

test("re-adds after sync never postpone synced", async () => {
	const broadcast = new BroadcastProducer();
	const lister = wireOf(broadcast).lister();
	const tracks = list(broadcast);
	lister.add("a", { info: INFO });
	lister.add("b", { info: INFO });
	expect(await tracks.drain()).toEqual(["+a", "+b"]);
	lister.synced();
	// Before each read, a track the reader saw leaves and comes back, in an order that kept a
	// re-add ahead of a counted SYNCED forever.
	const events: string[] = [];
	for (const name of ["a", "b", "a", "a", "b", "b", "a", "a"]) {
		if (events.includes("synced")) break;
		lister.remove(name);
		lister.add(name, { info: INFO });
		const event = await tracks.tracks.next();
		events.push(event?.kind === "synced" ? "synced" : `${event?.kind} ${event?.name}`);
	}
	expect(events).toEqual(["synced"]);
});

test("a name listed by both is reported once", async () => {
	const broadcast = new BroadcastProducer();
	defined(broadcast.createTrack("a"));
	const lister = wireOf(broadcast).lister();
	lister.add("a", { info: INFO });
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
	lister.add("up", { info: INFO });
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
	lister.add("up", { info: INFO });
	const tracks = list(broadcast);
	expect(await tracks.drain()).toEqual(["+up"]);
	lister.close();
	expect(await tracks.drain()).toEqual(["-up", "synced"]);
});

test("a closed lister answers nothing more, even beside its successor", async () => {
	const broadcast = new BroadcastProducer();
	const stale = wireOf(broadcast).lister();
	stale.close();
	const lister = wireOf(broadcast).lister();
	const tracks = list(broadcast);
	stale.add("stale", { info: INFO });
	stale.synced();
	stale.abort(new Error("stale"));
	expect(await tracks.drain()).toEqual([]);
	lister.add("up", { info: INFO });
	lister.synced();
	expect(await tracks.drain()).toEqual(["+up", "synced"]);
});

test("an owner lists each broadcast once", async () => {
	const broadcast = new BroadcastProducer();
	const other = new BroadcastProducer();
	defined(broadcast.createTrack("a"));
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
	defined(broadcast.createTrack("b"));
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
	lister.add("up", { info: INFO });
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
	defined(broadcast.createTrack("a"));
	defined(broadcast.createTrack("b"));
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
