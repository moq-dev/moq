import { afterEach, beforeEach, expect, jest, spyOn, test } from "bun:test";
import { Duration, type Installed } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import * as Announce from "../announced.ts";
import { AuthSession } from "../auth_session.ts";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "../broadcast.ts";
import { WireRoute } from "../hop.ts";
import * as Identity from "../identity.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { closeUnwanted } from "../util/announce.ts";
import { registerWire, wireOf } from "../wire.ts";
import { forwardAnnounced } from "./forward.ts";
import type { Session } from "./session.ts";

const LINGER = 2000;

let clock: Manual;
let installed: Installed;

beforeEach(() => {
	clock = new Manual();
	installed = clock.install();
	spyOn(console, "warn").mockImplementation(() => {});
});

afterEach(() => {
	installed.close();
	jest.restoreAllMocks();
});

/** Drain the reactive work without moving time. */
function settle(): Promise<void> {
	return clock.advance(Duration.ZERO);
}

/** Move time forward, draining the reactive work at each deadline on the way. */
function advance(ms: number): Promise<void> {
	return clock.advance(Duration.millis(ms));
}

/**
 * A session standing in for the wire: the test drives each announce request's stream and
 * decides when the session dies, which is what separates "discovery failed" from "the
 * session went away".
 */
class FakeSession {
	/** Every announce request the forwarder opened, by scope, in order. */
	readonly requests: {
		scope: string;
		hidden: boolean | undefined;
		covering: boolean | undefined;
		claims: boolean | undefined;
		stream: Announce.Producer;
	}[] = [];

	/**
	 * Every broadcast the forwarder consumed, per path. An announcement consumes the path
	 * once for the table, so a second consume is the blind answer to a request.
	 */
	readonly consumed = new Map<Path.Valid, BroadcastProducer[]>();

	/** The identity each consume was pinned to, in order. */
	readonly pins: (Identity.Valid | undefined)[] = [];

	/** Whether each request is synced as it opens, as one with no initial set to wait for is. */
	syncedAtOnce = false;

	/** Called as the forwarder closes a request, before its stream can wind down. */
	onClose?: () => void;

	closed: Promise<Error | null>;
	#die!: (reason: Error | null) => void;

	/** `exclusive` stands in for a peer that refuses overlapping requests. */
	constructor({ exclusive = false } = {}) {
		registerWire(this, {
			consume: (path, identity) => this.consume(path, identity),
			exclusive,
			leave: () => {},
			auth: new AuthSession({ peerGrant: { publish: new Path.Prefixes(), subscribe: new Path.Prefixes() } }),
		});
		this.closed = new Promise((resolve) => {
			this.#die = resolve;
		});
	}

	routes(scope: Path.Pattern = Path.Pattern.all(), options?: Announce.RouteOptions): Announce.Routes {
		const stream = new Announce.Producer();
		this.requests.push({
			scope: scope.text,
			hidden: options?.hidden,
			covering: options?.covering,
			claims: options?.claims,
			stream,
		});
		if (this.syncedAtOnce) stream.synced();
		const consumer = stream.consume();
		const close = consumer.close.bind(consumer);
		consumer.close = () => {
			this.onClose?.();
			close();
		};
		// The last reader leaving cancels the request, as a session's does.
		closeUnwanted(stream);
		return consumer;
	}

	/** The scopes of the announce requests still open. */
	open(): string[] {
		return this.requests.filter(({ stream }) => stream.closed.peek() === undefined).map(({ scope }) => scope);
	}

	/** The live stream for `scope`. */
	stream(scope: string): Announce.Producer {
		const found = this.requests.find(
			(request) => request.scope === scope && request.stream.closed.peek() === undefined,
		);
		if (!found) throw new Error(`no open announce request for ${scope}`);
		return found.stream;
	}

	consume(path: Path.Valid, identity?: Identity.Valid): BroadcastConsumer {
		this.pins.push(identity);
		const producer = new BroadcastProducer();
		const existing = this.consumed.get(path);
		if (existing) existing.push(producer);
		else this.consumed.set(path, [producer]);
		return producer.consume();
	}

	/** How many times the forwarder consumed `path` on this session. */
	consumes(path: Path.Valid): number {
		return this.consumed.get(path)?.length ?? 0;
	}

	/** End the session, the way a dropped connection would. */
	die(): void {
		this.#die(null);
	}

	/** What `forwardAnnounced` takes; only the members it touches are implemented. */
	get session(): Session {
		return this as unknown as Session;
	}
}

function start(stream: Announce.Producer, prefix: string, route: WireRoute = WireRoute.default) {
	stream.append({ prefix: Path.from(prefix), captures: undefined, kind: "start", route });
}

/** Every event an origin announcement stream yields, as `kind prefix`. */
function record(stream: Announce.Routes): string[] {
	const events: string[] = [];
	void (async () => {
		for await (const event of stream) events.push(`${event.kind} ${event.prefix}`);
	})();
	return events;
}

// A session can carry another identity's claim at the prefix, which ranks apart from this one:
// a request through a received route consumes that route's identity, never whichever wins.
test("a request through a received route consumes that route's identity", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);
	const identity = Identity.parse("a");
	const request = origin.request(Path.from("pool/job"));
	await settle();
	start(session.stream("pool/job/**"), "pool/job", { ...WireRoute.default, identity });
	session.stream("pool/job/**").synced();
	await settle();
	expect(request.active.peek()).toBeDefined();
	expect(session.pins).toEqual([identity]);
	request.close();
	session.die();
	origin.close();
});

// A lite-07 stream reports each identity's claim at a path as its own route, so the origin holds
// every candidate: an unpinned request spreads by identity, and one pinned to the claim that
// does not win still resolves.
test("the origin holds each identity's claim at a prefix", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);
	const a = Identity.parse("a");
	const b = Identity.parse("b");
	const watch = origin.routes(Path.Pattern.subtree(Path.from("pool")));
	await settle();
	expect(session.requests[0]).toMatchObject({ claims: true });
	start(session.stream("pool/**"), "pool", { ...WireRoute.default, identity: a, claimed: true });
	start(session.stream("pool/**"), "pool", { ...WireRoute.default, identity: b, claimed: true });
	session.stream("pool/**").synced();
	await settle();

	// `b`'s rendezvous hash wins `pool/job`; see `lite07_ties_match_js` in rs/moq-net.
	const job = Path.from("pool/job");
	const unpinned = origin.request(job);
	await settle();
	expect(unpinned.active.peek()).toBeDefined();
	expect(session.pins).toEqual([b]);
	const pinned = origin.request(job, { identity: a });
	await settle();
	expect(pinned.active.peek()).toBeDefined();
	expect(session.pins).toEqual([b, a]);

	unpinned.close();
	pinned.close();
	watch.close();
	session.die();
	origin.close();
});

// A route handed to another request's report of another identity restarts, so its requests
// consume that identity rather than the one the first report named.
test("a route handed over to another identity consumes that identity", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);
	const a = Identity.parse("a");
	const b = Identity.parse("b");
	const narrow = origin.routes(Path.Pattern.subtree(Path.from("a/b")));
	await settle();
	start(session.stream("a/b/**"), "a/b/x", { ...WireRoute.default, identity: a, epoch: 0n });
	session.stream("a/b/**").synced();
	const broad = origin.routes(Path.Pattern.subtree(Path.from("a")));
	await settle();
	start(session.stream("a/**"), "a/b/x", { ...WireRoute.default, identity: b, epoch: 0n });
	session.stream("a/**").synced();
	await settle();

	session.stream("a/b/**").close(new Error("namespace rejected"));
	await settle();
	const request = origin.request(Path.from("a/b/x"));
	await settle();
	expect(request.active.peek()).toBeDefined();
	expect(session.pins.at(-1)).toBe(b);

	request.close();
	narrow.close();
	broad.close();
	session.die();
	origin.close();
});

test("a request discovers an unwatched path and waits for its initial set", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);
	expect(session.requests).toEqual([]);
	const request = origin.request(Path.from("room"));
	await settle();
	expect(session.open()).toEqual(["room/**"]);
	// The origin routes through every route the request reports, a covering one included.
	expect(session.requests[0]).toMatchObject({ hidden: true, covering: true });
	expect(request.active.peek()).toBeUndefined();
	expect(session.consumes(Path.from("room"))).toBe(0);
	start(session.stream("room/**"), "room");
	session.stream("room/**").synced();
	await settle();
	expect(request.active.peek()).toBeDefined();
	expect(session.consumes(Path.from("room"))).toBe(1);
	request.close();
	session.die();
	origin.close();
});

test("a watch opens its request, and the request closes once the linger expires", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);

	const watch = origin.routes(Path.Pattern.subtree(Path.from("room")));
	await settle();
	expect(session.requests.map(({ scope, hidden }) => ({ scope, hidden }))).toEqual([
		{ scope: "room/**", hidden: true },
	]);

	// Unwatching keeps the request open for the linger, so a watcher rebuilt in that window reuses it.
	watch.close();
	await advance(LINGER - 1);
	expect(session.open()).toEqual(["room/**"]);

	const again = origin.routes(Path.Pattern.subtree(Path.from("room")));
	await advance(LINGER);
	expect(session.open()).toEqual(["room/**"]);
	expect(session.requests.length).toBe(1);

	again.close();
	await advance(LINGER);
	expect(session.open()).toEqual([]);
	// Closing it on purpose is not a discovery failure.
	expect(origin.discovery.peek()).toBe(true);

	session.die();
	origin.close();
});

test("a watch arriving as the linger expires opens a fresh request", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);

	const watch = origin.routes(Path.Pattern.subtree(Path.from("room")));
	await settle();
	watch.close();
	await settle();

	// The expiry closes the old request, and the next watch lands before its stream winds down.
	let again: Announce.Routes | undefined;
	session.onClose = () => {
		again ??= origin.routes(Path.Pattern.subtree(Path.from("room")));
	};
	await advance(LINGER);
	expect(again).toBeDefined();
	expect(session.requests.length).toBe(2);
	expect(session.open()).toEqual(["room/**"]);

	again?.close();
	session.die();
	origin.close();
});

test("a watch change reports only its prefix and the watches it covers", () => {
	const origin = new OriginProducer();
	const events: string[] = [];
	const unlisten = wireOf(origin).interests((prefix, wanted) => events.push(`${wanted ? "+" : "-"}${prefix}`));
	const state = origin.consume();

	const ab = state.follow(Path.from("a/b"));
	const ac = state.follow(Path.from("a/c"));
	const x = state.follow(Path.from("x"));
	expect(events.splice(0)).toEqual(["+a/b", "+a/c", "+x"]);

	// The broader watch opens first, then releases the ones it now covers; unrelated `x` is untouched.
	const a = state.follow(Path.from("a"));
	expect(events.splice(0)).toEqual(["+a", "-a/b", "-a/c"]);

	// A watch beneath a covered prefix changes nothing.
	const abd = state.follow(Path.from("a/b/d"));
	abd.close();
	expect(events.splice(0)).toEqual([]);

	a.close();
	expect(events.splice(0)).toEqual(["+a/b", "+a/c", "-a"]);

	// A late listener hears the current covering set first.
	const late: string[] = [];
	const stop = wireOf(origin).interests((prefix, wanted) => late.push(`${wanted ? "+" : "-"}${prefix}`));
	expect(late).toEqual(["+a/b", "+a/c", "+x"]);

	stop();
	unlisten();
	for (const request of [ab, ac, x]) request.close();
	origin.close();
});

test("a follower is a watch on its path", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);

	const request = origin.follow(Path.from("room/alice"));
	await settle();
	expect(session.open()).toEqual(["room/alice/**"]);
	// Gated on the announcement, so nothing answers it blind meanwhile.
	expect(request.active.peek()).toBeUndefined();

	start(session.stream("room/alice/**"), "room/alice");
	await settle();
	expect(request.active.peek()).toBeDefined();

	request.close();
	await advance(LINGER);
	expect(session.open()).toEqual([]);

	session.die();
	origin.close();
});

test("a pattern watches its literal head", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);

	const chat = origin.routes(Path.Pattern.parse("room/*/chat"));
	await settle();
	expect(session.open()).toEqual(["room/**"]);

	chat.close();
	session.die();
	origin.close();
});

test("a narrower watch rides an open broader request and keeps it open", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);

	const broad = origin.routes(Path.Pattern.subtree(Path.from("a")));
	const narrow = origin.routes(Path.Pattern.subtree(Path.from("a/b")));
	const events = record(narrow);
	await settle();
	expect(session.open()).toEqual(["a/**"]);

	start(session.stream("a/**"), "a/b/x");
	start(session.stream("a/**"), "a/c");
	await settle();
	expect(events).toEqual(["start a/b/x"]);

	// The broader watch leaves, but its request stays open for the narrower one, so no request
	// takes its routes over and nothing restarts.
	broad.close();
	await advance(LINGER);
	expect(session.open()).toEqual(["a/**"]);
	expect(wireOf(origin).routes(Path.from("a/b/x"))).toBe(true);
	expect(events).toEqual(["start a/b/x"]);

	// Once nothing beneath it is watched, it lingers out.
	narrow.close();
	await advance(LINGER);
	expect(session.open()).toEqual([]);
	expect(wireOf(origin).routes(Path.from("a/b/x"))).toBe(false);

	session.die();
	origin.close();
});

test("a narrower request stays open until the broader one covering it goes", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);

	const narrow = origin.routes(Path.Pattern.subtree(Path.from("a/b")));
	await settle();
	start(session.stream("a/b/**"), "a/b/x");
	await settle();

	// A broader watch opens its own request, which has yet to answer when the narrower watch
	// leaves. The narrower request stays open, so the route never retracts.
	const broad = origin.routes(Path.Pattern.subtree(Path.from("a")));
	const events = record(broad);
	await settle();
	narrow.close();
	await advance(LINGER);
	expect(session.open()).toEqual(["a/b/**", "a/**"]);
	expect(wireOf(origin).routes(Path.from("a/b/x"))).toBe(true);
	expect(events).toEqual(["start a/b/x"]);

	// Once the broader request lingers out, the narrower one it held follows.
	broad.close();
	await advance(LINGER);
	await advance(LINGER);
	expect(session.open()).toEqual([]);
	expect(wireOf(origin).routes(Path.from("a/b/x"))).toBe(false);

	session.die();
	origin.close();
});

test("an exclusive session closes every request beneath a broader one before it opens", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession({ exclusive: true });
	forwardAnnounced(session.session, origin);

	const narrow = origin.routes(Path.Pattern.subtree(Path.from("a/b")));
	const lingering = origin.routes(Path.Pattern.subtree(Path.from("a/c")));
	await settle();
	start(session.stream("a/b/**"), "a/b/x");
	lingering.close();
	await settle();
	expect(session.open()).toEqual(["a/b/**", "a/c/**"]);

	// Watched or lingering, each narrower request closes for the broader one, on purpose: the
	// session stays discovering, and the route returns once the broader request reports it.
	const broad = origin.routes(Path.Pattern.subtree(Path.from("a")));
	const events = record(broad);
	await settle();
	expect(session.open()).toEqual(["a/**"]);
	expect(wireOf(origin).routes(Path.from("a/b/x"))).toBe(false);
	start(session.stream("a/**"), "a/b/x");
	await settle();
	expect(events).toEqual(["start a/b/x", "offline a/b/x", "start a/b/x"]);
	expect(origin.discovery.peek()).toBe(true);

	// The narrower watch rides the broader request from here on.
	broad.close();
	await advance(LINGER);
	expect(session.open()).toEqual(["a/**"]);
	narrow.close();
	await advance(LINGER);
	expect(session.open()).toEqual([]);
	expect(session.requests.map(({ scope }) => scope)).toEqual(["a/b/**", "a/c/**", "a/**"]);

	session.die();
	origin.close();
});

const first = Identity.random();
for (const [name, identities] of [
	["no identity", [undefined, undefined]],
	["one identity", [first, first]],
	["another identity", [first, Identity.random()]],
] as const) {
	test(`a route changing hands restarts unless both routes carry one identity (${name})`, async () => {
		const origin = new OriginProducer();
		const session = new FakeSession();
		forwardAnnounced(session.session, origin);

		const narrow = origin.routes(Path.Pattern.subtree(Path.from("a/b")));
		await settle();
		start(session.stream("a/b/**"), "a/b/x", { ...WireRoute.default, identity: identities[0] });
		await settle();
		const broad = origin.routes(Path.Pattern.subtree(Path.from("a")));
		const events = record(broad);
		await settle();
		start(session.stream("a/**"), "a/b/x", { ...WireRoute.default, identity: identities[1] });
		await settle();
		expect(events).toEqual(["start a/b/x"]);

		// The narrower request, which drives the route, fails; the broader one takes it over.
		session.stream("a/b/**").close(new Error("reset"));
		await settle();
		expect(wireOf(origin).routes(Path.from("a/b/x"))).toBe(true);
		expect(events).toEqual(name === "one identity" ? ["start a/b/x"] : ["start a/b/x", "restart a/b/x"]);

		narrow.close();
		broad.close();
		session.die();
		origin.close();
	});
}

test("a reconnect re-opens every live watch and skips a lingering one", async () => {
	const origin = new OriginProducer();
	const first = new FakeSession();
	forwardAnnounced(first.session, origin);

	const kept = origin.routes(Path.Pattern.subtree(Path.from("kept")));
	const dropped = origin.routes(Path.Pattern.subtree(Path.from("dropped")));
	await settle();
	expect(first.open()).toEqual(["kept/**", "dropped/**"]);

	// Still lingering on the first session when it dies.
	dropped.close();
	await settle();
	first.die();
	await settle();
	expect(first.open()).toEqual([]);

	const second = new FakeSession();
	forwardAnnounced(second.session, origin);
	await settle();
	expect(second.open()).toEqual(["kept/**"]);

	kept.close();
	second.die();
	origin.close();
});

test("a refused announce request downgrades the session once, and later watches open nothing", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	const path = Path.from("room");
	forwardAnnounced(session.session, origin);

	const watched = origin.follow(path);
	await settle();
	expect(origin.discovery.peek()).toBe(true);
	expect(watched.active.peek()).toBeUndefined();

	// The relay refuses the request but keeps the session.
	session.stream("room/**").close(new Error("namespace rejected"));
	await settle();
	expect(origin.discovery.peek()).toBe(false);
	// Following cannot use a peer that no longer announces; a plain request can.
	expect(watched.active.peek()).toBeUndefined();
	expect(session.consumes(path)).toBe(0);
	const plain = origin.request(path);
	await settle();
	expect(plain.active.peek()).toBeDefined();
	expect(session.consumes(path)).toBe(1);

	const later = origin.routes(Path.Pattern.subtree(Path.from("other")));
	await settle();
	expect(session.requests.length).toBe(1);

	later.close();
	plain.close();
	watched.close();
	session.die();
	origin.close();
});

test("a discovery failure under a live session retracts what it fed and answers blind", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	const path = Path.from("room");
	forwardAnnounced(session.session, origin);

	const watched = origin.follow(path);
	await settle();
	start(session.stream("room/**"), "room");
	await settle();
	expect(wireOf(origin).routes(path)).toBe(true);
	expect(watched.active.peek()).toBeDefined();

	// The relay resets the stream but keeps the session: a subscriber is allowed to refuse a
	// namespace without closing the connection.
	session.stream("room/**").close(new Error("stream reset"));
	await settle();

	// Everything the stream fed is retracted, and the origin stops claiming a discovery that
	// no longer works. Leaving it true is what would strand every gated watcher offline.
	expect(wireOf(origin).routes(path)).toBe(false);
	expect(origin.discovery.peek()).toBe(false);

	// The follower stays offline; a separate plain request can use the blind peer.
	expect(watched.closed.peek()).toBeUndefined();
	expect(watched.active.peek()).toBeUndefined();
	const plain = origin.request(path);
	await settle();
	expect(plain.active.peek()).toBeDefined();
	// The announcement's own consume, plus the blind answer standing in for it now.
	expect(session.consumes(path)).toBe(2);

	watched.close();
	plain.close();
	session.die();
	origin.close();
});

test("a session dying does not downgrade discovery for the next one", async () => {
	const origin = new OriginProducer();
	const first = new FakeSession();
	forwardAnnounced(first.session, origin);
	const watch = origin.routes();
	await settle();
	expect(origin.discovery.peek()).toBe(true);

	// A closing session detaches outright rather than downgrading: it is gone, not blind.
	first.die();
	await settle();
	expect(origin.discovery.peek()).toBeUndefined();

	const second = new FakeSession();
	forwardAnnounced(second.session, origin);
	expect(origin.discovery.peek()).toBe(true);
	await settle();
	expect(second.open()).toEqual(["**"]);

	watch.close();
	second.die();
	origin.close();
});

test("a request replaced across one coalesced wakeup still gets answered", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	const path = Path.from("wanted");

	forwardAnnounced(session.session, origin);
	// Refusing discovery downgrades the session, so plain requests take its blind answers.
	const listing = origin.routes(Path.Pattern.subtree(path));
	await settle();
	session.stream("wanted/**").close(new Error("namespace rejected"));
	await settle();

	const first = origin.request(path);
	await settle();
	expect(first.active.peek()).toBeDefined();
	expect(session.consumes(path)).toBe(1);

	// Withdrawing the last handle defers the slot teardown a microtask. Let that teardown
	// run, then ask again before the serving loop wakes, so the delete and the fresh slot
	// land in one notification. The loop sees a slot it never answered under a path it did.
	first.close();
	await Promise.resolve();
	await Promise.resolve();
	const second = origin.request(path);
	await settle();

	expect(second.active.peek()).toBeDefined();
	expect(session.consumes(path)).toBe(2);

	second.close();
	listing.close();
	session.die();
	origin.close();
});

test("a scoped origin narrows a root watch to its prefixes", async () => {
	const origin = new OriginProducer();
	const scoped = origin.scope(Path.empty(), Path.Prefixes.parse(["room", "room/nested", "other"]));
	const session = new FakeSession();
	forwardAnnounced(session.session, scoped);

	const all = origin.routes();
	await settle();
	expect(session.open()).toEqual(["other/**", "room/**"]);
	all.close();

	// A watch inside a prefix rides the prefix's open request, and one outside the scope asks
	// for nothing.
	let inside = origin.routes(Path.Pattern.subtree(Path.from("room/alice/chat")));
	const outside = origin.routes(Path.Pattern.subtree(Path.from("elsewhere")));
	await advance(LINGER);
	expect(session.open()).toEqual(["room/**"]);

	// With nothing open above it, a watch inside a prefix asks for itself.
	inside.close();
	await advance(LINGER);
	expect(session.open()).toEqual([]);
	inside = origin.routes(Path.Pattern.subtree(Path.from("room/alice/chat")));
	await settle();
	expect(session.open()).toEqual(["room/alice/chat/**"]);

	inside.close();
	outside.close();
	session.die();
	origin.close();
});

test("a scoped session filters discovered routes and requests under its root", async () => {
	const origin = new OriginProducer();
	const scoped = origin.scope(Path.from("tenant"), Path.Prefixes.parse(["room/alice/chat", "room/bob/chat"]));
	const session = new FakeSession();
	forwardAnnounced(session.session, scoped);

	const all = origin.routes();
	const events = record(all);
	await settle();
	// Session-relative: the scope's root is the session's, and it asks for each prefix.
	expect(session.open()).toEqual(["room/alice/chat/**", "room/bob/chat/**"]);
	start(session.stream("room/alice/chat/**"), "room/alice/chat");
	start(session.stream("room/bob/chat/**"), "room/bob/chat");

	const allowed = origin.request(Path.from("tenant/room/bob/chat"));
	const denied = origin.request(Path.from("tenant/room/bob/video"));
	await settle();
	expect(events).toEqual(["start tenant/room/alice/chat", "start tenant/room/bob/chat"]);
	expect(session.consumes(Path.from("room/bob/chat"))).toBe(1);
	expect(session.consumes(Path.from("room/bob/video"))).toBe(0);

	const routed = origin.request(Path.from("tenant/room/alice/chat"));
	await settle();
	expect(session.consumes(Path.from("room/alice/chat"))).toBe(1);
	expect(routed.active.peek()).toBeDefined();

	allowed.close();
	denied.close();
	routed.close();
	all.close();
	session.die();
	origin.close();
});

test("one failed request leaves the other routes live until the session ends", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);

	const a = origin.routes(Path.Pattern.subtree(Path.from("a")));
	const b = origin.routes(Path.Pattern.subtree(Path.from("b")));
	await settle();
	start(session.stream("a/**"), "a");
	start(session.stream("b/**"), "b");
	await settle();
	expect(wireOf(origin).routes(Path.from("a"))).toBe(true);

	session.stream("a/**").close(new Error("namespace rejected"));
	await settle();
	expect(wireOf(origin).routes(Path.from("a"))).toBe(false);
	expect(wireOf(origin).routes(Path.from("b"))).toBe(true);
	expect(origin.discovery.peek()).toBe(false);

	session.die();
	await settle();
	expect(wireOf(origin).routes(Path.from("b"))).toBe(false);
	expect(origin.discovery.peek()).toBeUndefined();

	a.close();
	b.close();
	origin.close();
});

test("an origin watcher syncs with the session's initial set, not before", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);

	const watch = origin.routes(Path.Pattern.subtree(Path.from("room")));
	const events = record(watch);
	await settle();
	expect(watch.synced.peek()).toBe(false);

	const stream = session.stream("room/**");
	start(stream, "room/cam");
	await settle();
	// The route is in, but the peer has not said the set is complete.
	expect(events).toEqual(["start room/cam"]);
	expect(watch.synced.peek()).toBe(false);

	stream.synced();
	await settle();
	expect(watch.synced.peek()).toBe(true);

	watch.close();
	session.die();
	origin.close();
});

test("a session ending before its initial set lands stops holding the watcher", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	forwardAnnounced(session.session, origin);

	const watch = origin.routes();
	record(watch);
	await settle();
	expect(watch.synced.peek()).toBe(false);

	session.die();
	await settle();
	expect(watch.synced.peek()).toBe(true);

	watch.close();
	origin.close();
});

test("a request synced as it opens releases the watcher", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	session.syncedAtOnce = true;
	forwardAnnounced(session.session, origin);

	const watch = origin.routes();
	record(watch);
	await settle();
	expect(session.requests).toHaveLength(1);
	expect(watch.synced.peek()).toBe(true);

	watch.close();
	session.die();
	origin.close();
});

test("a scoped session refuses blind requests outside its allowed paths", async () => {
	const origin = new OriginProducer();
	const scoped = origin.scope(Path.from("tenant"), Path.Prefixes.parse(["room/bob/chat"]));
	const session = new FakeSession();
	forwardAnnounced(session.session, scoped);
	const allowed = origin.request(Path.from("tenant/room/bob/chat"));
	const denied = origin.request(Path.from("tenant/room/bob/video"));
	await settle();
	for (const request of session.requests) request.stream.close(new Error("no discovery"));
	await settle();
	expect(allowed.active.peek()).toBeDefined();
	expect(session.consumes(Path.from("room/bob/chat"))).toBe(1);
	expect(denied.active.peek()).toBeUndefined();
	expect(session.consumes(Path.from("room/bob/video"))).toBe(0);
	allowed.close();
	denied.close();
	session.die();
	origin.close();
});
