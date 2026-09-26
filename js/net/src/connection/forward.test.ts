import { expect, test } from "bun:test";
import * as Announce from "../announced.ts";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "../broadcast.ts";
import { Route } from "../hop.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { registerWire, wireOf } from "../wire.ts";
import type { Established } from "./established.ts";
import { forwardAnnounced } from "./forward.ts";

async function settle() {
	await new Promise((resolve) => setTimeout(resolve, 0));
}

/**
 * A session standing in for the wire: the test drives its announcement stream and decides
 * when it dies, which is what separates "discovery failed" from "the session went away".
 */
class FakeSession {
	readonly discovery: boolean;

	/** The announce stream handed to the forwarder, so a test can end or abort it. */
	readonly announces = new Announce.Producer();

	/**
	 * Every broadcast the forwarder consumed, per path. An announcement consumes the path
	 * once for the table, so a second consume is the blind answer to a request.
	 */
	readonly consumed = new Map<Path.Valid, BroadcastProducer[]>();

	closed: Promise<Error | null>;
	#die!: (reason: Error | null) => void;

	constructor(discovery = true) {
		this.discovery = discovery;
		registerWire(this, { consume: (path) => this.consume(path) });
		this.closed = new Promise((resolve) => {
			this.#die = resolve;
		});
	}

	announced(_scope?: Path.Pattern, _options?: Announce.Options): Announce.Consumer {
		return this.announces.consume();
	}

	consume(path: Path.Valid): BroadcastConsumer {
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
	get session(): Established {
		return this as unknown as Established;
	}
}

test("a discovery failure under a live session downgrades the origin", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	const path = Path.from("room");

	forwardAnnounced(session.session, origin);

	// The relay announces a broadcast, which lands in the table.
	session.announces.append({ prefix: path, captures: undefined, kind: "announced", route: Route.default });
	await settle();
	expect(origin.discovery.peek()).toBe(true);
	expect(wireOf(origin).routes(path)).toBe(true);

	// A watcher gated on the announcement is live on that route.
	const watched = origin.request(path, { announced: true });
	await settle();
	await settle();
	expect(watched.active.peek()).toBeDefined();

	// The relay resets the announce stream but keeps the session: a subscriber is allowed to
	// refuse a namespace without closing the connection.
	session.announces.close(new Error("namespace rejected"));
	await settle();
	await settle();

	// Everything the stream fed is retracted, and the origin stops claiming a discovery that
	// no longer works. Leaving it true is what used to strand every gated watcher offline.
	expect(wireOf(origin).routes(path)).toBe(false);
	expect(origin.discovery.peek()).toBe(false);

	// So the watcher falls back to a standing request, which this same session answers.
	await settle();
	await settle();
	expect(watched.active.peek()).toBeDefined();
	// The announcement's own consume, plus the blind answer standing in for it now.
	expect(session.consumes(path)).toBe(2);

	watched.close();
	origin.close();
});

test("a request outlives the discovery failure that fed it", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	const path = Path.from("wanted");

	forwardAnnounced(session.session, origin);

	// Announced, so the table routes it and no blind answer is needed.
	session.announces.append({ prefix: path, captures: undefined, kind: "announced", route: Route.default });
	await settle();

	const request = origin.request(path);
	await settle();
	expect(request.active.peek()).toBeDefined();
	// Only the announcement's consume: a routed path resolves the request on its own, so the
	// serving loop leaves it alone rather than parking a blind subscription behind it.
	expect(session.consumes(path)).toBe(1);

	// Discovery dies under the live session: the route goes, so the request now needs the
	// blind answer the serving loop skipped while the table had it.
	session.announces.close(new Error("stream reset"));
	await settle();
	await settle();

	expect(request.active.peek()).toBeDefined();
	expect(session.consumes(path)).toBe(2);

	request.close();
	origin.close();
});

test("a session dying does not downgrade discovery for the next one", async () => {
	const origin = new OriginProducer();
	const first = new FakeSession();

	forwardAnnounced(first.session, origin);
	expect(origin.discovery.peek()).toBe(true);

	// A closing session detaches outright rather than downgrading: it is gone, not blind.
	first.die();
	await settle();
	await settle();
	expect(origin.discovery.peek()).toBeUndefined();

	const second = new FakeSession();
	forwardAnnounced(second.session, origin);
	expect(origin.discovery.peek()).toBe(true);

	second.die();
	origin.close();
});

test("a request replaced across one coalesced wakeup still gets answered", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession(false);
	const path = Path.from("wanted");

	forwardAnnounced(session.session, origin);

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
	await settle();

	expect(second.active.peek()).toBeDefined();
	expect(session.consumes(path)).toBe(2);

	second.close();
	origin.close();
});

test("scoped origins subscribe to disjoint literal heads with hidden routes", () => {
	const origin = new OriginProducer().scope(
		Path.empty(),
		new Path.Patterns(["room/*/chat", "room/nested/**", "other/**"].map(Path.Pattern.parse)),
	);
	const session = new FakeSession();
	const interests: { scope: string | undefined; hidden: boolean | undefined }[] = [];
	session.announced = (scope?: Path.Pattern, options?: Announce.Options) => {
		interests.push({ scope: scope?.text, hidden: options?.hidden });
		return session.announces.consume();
	};
	forwardAnnounced(session.session, origin);
	expect(interests).toEqual([
		{ scope: "other/**", hidden: true },
		{ scope: "room/**", hidden: true },
	]);
	session.die();
	origin.close();
});

test("an unscoped origin retains its subscription to every namespace", () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	const interests: string[] = [];
	session.announced = (scope?: Path.Pattern) => {
		interests.push(scope?.text ?? "**");
		return session.announces.consume();
	};
	forwardAnnounced(session.session, origin);
	expect(interests).toEqual(["**"]);
	session.die();
	origin.close();
});

test("a scoped session filters announcements and blind requests under its root", async () => {
	const origin = new OriginProducer();
	const scoped = origin.scope(Path.from("tenant"), new Path.Patterns([Path.Pattern.parse("room/*/chat")]));
	const session = new FakeSession();
	forwardAnnounced(session.session, scoped);
	for (const prefix of ["room/alice/video", "room/alice/chat"]) {
		session.announces.append({
			prefix: Path.from(prefix),
			captures: undefined,
			kind: "announced",
			route: Route.default,
		});
	}
	const allowed = origin.request(Path.from("tenant/room/bob/chat"));
	const denied = origin.request(Path.from("tenant/room/bob/video"));
	await settle();
	expect([...origin.broadcasts().peek().keys()]).toEqual([Path.from("tenant/room/alice/chat")]);
	expect(session.consumes(Path.from("room/bob/chat"))).toBe(1);
	expect(session.consumes(Path.from("room/bob/video"))).toBe(0);
	const routed = origin.request(Path.from("tenant/room/alice/chat"));
	await settle();
	expect(session.consumes(Path.from("room/alice/chat"))).toBe(1);
	expect(routed.active.peek()).toBeDefined();
	allowed.close();
	denied.close();
	routed.close();
	session.die();
	origin.close();
});

test("one failed scoped interest leaves the other routes live until the session ends", async () => {
	const origin = new OriginProducer().scope(
		Path.empty(),
		new Path.Patterns(["a/**", "b/**"].map(Path.Pattern.parse)),
	);
	const session = new FakeSession();
	const sources = new Map<string, Announce.Producer>();
	session.announced = (scope?: Path.Pattern) => {
		const producer = new Announce.Producer();
		sources.set(scope?.text ?? "**", producer);
		return producer.consume();
	};
	forwardAnnounced(session.session, origin);
	for (const name of ["a", "b"]) {
		sources
			.get(`${name}/**`)
			?.append({ prefix: Path.from(name), captures: undefined, kind: "announced", route: Route.default });
	}
	await settle();
	expect(origin.broadcasts().peek().size).toBe(2);
	sources.get("a/**")?.close(new Error("namespace rejected"));
	await settle();
	expect([...origin.broadcasts().peek().keys()]).toEqual([Path.from("b")]);
	expect(origin.discovery.peek()).toBe(false);
	session.die();
	await settle();
	expect(origin.broadcasts().peek().size).toBe(0);
	expect(origin.discovery.peek()).toBeUndefined();
	origin.close();
});
