import { expect, test } from "bun:test";
import * as Announce from "../announced.ts";
import { type Consumer as BroadcastConsumer, Producer as BroadcastProducer } from "../broadcast.ts";
import { Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
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

	/**
	 * Every prefix the forwarder solicited, in order. Empty until something is interested,
	 * which is what makes the laziness observable.
	 */
	readonly requested: Path.Valid[] = [];

	// One producer per prefix, created on demand so a test can queue announcements before
	// the forwarder gets around to asking for them.
	readonly #streams = new Map<Path.Valid, Announce.Producer>();

	/**
	 * Every broadcast the forwarder consumed, per path. An announcement consumes the path
	 * once for the table, so a second consume is the blind answer to a request.
	 */
	readonly consumed = new Map<Path.Valid, BroadcastProducer[]>();

	closed: Promise<Error | null>;
	#die!: (reason: Error | null) => void;

	constructor(discovery = true) {
		this.discovery = discovery;
		this.closed = new Promise((resolve) => {
			this.#die = resolve;
		});
	}

	/** The stream for `prefix`, so a test can drive, end, or abort it. */
	stream(prefix: Path.Valid = Path.empty()): Announce.Producer {
		const existing = this.#streams.get(prefix);
		if (existing) return existing;
		const producer = new Announce.Producer(prefix);
		this.#streams.set(prefix, producer);
		return producer;
	}

	/** The root stream, which is what a plain `origin.announced()` solicits. */
	get announces(): Announce.Producer {
		return this.stream();
	}

	announced(prefix: Path.Valid = Path.empty()): Announce.Consumer {
		this.requested.push(prefix);
		return this.stream(prefix).consume();
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

	// Something has to be listening before anything is solicited; this handle is what makes
	// the forwarder open the root stream at all.
	const listening = origin.announced();
	await settle();

	// The relay announces a broadcast, which lands in the table.
	session.announces.append({ path, active: true });
	await settle();
	expect(origin.discovery.peek()).toBe(true);
	expect(origin.routes(path)).toBe(true);

	// A watcher gated on the announcement is live on that route.
	const watched = new Announce.Broadcast({ origin, path });
	await settle();
	expect(watched.active.peek()).toBeDefined();

	// The relay resets the announce stream but keeps the session: a subscriber is allowed to
	// refuse a namespace without closing the connection.
	session.announces.close(new Error("namespace rejected"));
	await settle();
	await settle();

	// Everything the stream fed is retracted, and the origin stops claiming a discovery that
	// no longer works. Leaving it true is what used to strand every gated watcher offline.
	expect(origin.routes(path)).toBe(false);
	expect(origin.discovery.peek()).toBe(false);

	// So the watcher falls back to a standing request, which this same session answers.
	await settle();
	await settle();
	expect(watched.active.peek()).toBeDefined();
	// The announcement's own consume, plus the blind answer standing in for it now.
	expect(session.consumes(path)).toBe(2);

	watched.close();
	listening.close();
	origin.close();
});

test("a request outlives the discovery failure that fed it", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	const path = Path.from("wanted");

	forwardAnnounced(session.session, origin);

	// Interest is what opens the stream; without it nothing is solicited.
	const listening = origin.announced();
	await settle();

	// Announced, so the table routes it and no blind answer is needed.
	session.announces.append({ path, active: true });
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
	listening.close();
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

test("nothing is solicited until something listens", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	const path = Path.from("room");

	forwardAnnounced(session.session, origin);
	await settle();

	// A session that only publishes, or only resolves known paths by request, must cost the
	// peer no announce traffic at all. A request is not interest: the serving loop answers it
	// with a blind subscription, which needs no announcement.
	const request = origin.request(path);
	await settle();
	expect(session.requested).toEqual([]);
	expect(request.active.peek()).toBeDefined();

	// The first listener opens the stream, once.
	const listening = origin.announced();
	await settle();
	expect(session.requested).toEqual([Path.empty()]);

	const second = origin.announced();
	await settle();
	expect(session.requested).toEqual([Path.empty()]);

	second.close();
	listening.close();
	request.close();
	origin.close();
});

test("a narrow listener asks for its prefix, and a broader one opens its own stream", async () => {
	const origin = new OriginProducer();
	const session = new FakeSession();
	const prefix = Path.from("room", "a");
	const path = Path.from("room", "a", "cam");

	forwardAnnounced(session.session, origin);

	// Watching one room asks for that room, not the whole relay.
	const narrow = origin.announced(prefix);
	await settle();
	expect(session.requested).toEqual([prefix]);

	// Paths arrive relative to the stream's prefix and land absolute in the table.
	session.stream(prefix).append({ path: Path.from("cam"), active: true });
	await settle();
	expect(origin.routes(path)).toBe(true);
	expect(session.consumes(path)).toBe(1);

	// A root listener is not covered by the narrow stream, so it opens its own. The narrow one
	// stays: closing it would retract the routes it feeds out from under whoever is reading.
	const broad = origin.announced();
	await settle();
	expect(session.requested).toEqual([prefix, Path.empty()]);

	// The same path from both streams is one table entry, so nothing re-subscribes...
	session.announces.append({ path, active: true });
	await settle();
	expect(origin.routes(path)).toBe(true);

	// ...and it survives until the last stream announcing it retracts.
	session.stream(prefix).append({ path: Path.from("cam"), active: false });
	await settle();
	expect(origin.routes(path)).toBe(true);

	session.announces.append({ path, active: false });
	await settle();
	expect(origin.routes(path)).toBe(false);

	// A prefix under one already open is covered; no third stream.
	const nested = origin.announced(path);
	await settle();
	expect(session.requested).toEqual([prefix, Path.empty()]);

	nested.close();
	narrow.close();
	broad.close();
	origin.close();
});
