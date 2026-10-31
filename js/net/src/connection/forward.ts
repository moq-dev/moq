/**
 * Feeds a session's announced routes into an origin; the `consume` connect option.
 *
 * @module
 */

import { Milli, Nano } from "@moq/clock";
import { type Dispose, race } from "@moq/signals";
import type * as Announce from "../announced.ts";
import { type Route, routesEqual } from "../hop.ts";
import { coveringPrefixes, hooks } from "../internal.ts";
import type { Producer as OriginProducer, RequestSlot, Requests } from "../origin.ts";
import * as Path from "../path.ts";
import { after } from "../util/timer.ts";
import { wireOf } from "../wire.ts";
import type { Established } from "./established.ts";

/** Why a connection refuses the removed `discovery` option rather than ignoring it. */
export const DISCOVERY_REMOVED =
	"the discovery option is removed: a session asks only about watched prefixes and turns blind if the relay refuses";

/** How long an announce request outlives its last watcher, so a watcher torn down and rebuilt reuses it. */
const LINGER = Milli(2000);

/** One announce request open on the session, and the routes it currently reports. */
interface Interest {
	readonly announced: Announce.Consumer;
	readonly reported: Set<Path.Valid>;
	/** The pending close, set while nothing at or beneath the prefix is watched. */
	linger?: () => void;
	/** Closed on purpose, so its end is not a discovery failure. */
	closing: boolean;
	/** Narrower requests nothing watches any more, kept open until this one goes. */
	readonly held: Map<Path.Valid, Interest>;
}

/**
 * A route one or more overlapping requests report, inserted into the origin once so a
 * request handing over to another never retracts it. The first reporter drives it.
 */
interface Reported {
	handle: Requests;
	/** Each request's latest route for the prefix. */
	readonly reporters: Map<Interest, Route>;
}

/**
 * Wire a session into `origin` for the session's lifetime: forward the peer's announced
 * routes under the prefixes the origin's readers watch, and answer the origin's open
 * requests with blind subscriptions.
 *
 * Discovery is lazy. One announce request opens per prefix in the origin's covering set of
 * watches (`announced(scope)` and every `request(path)`) that no open request
 * already covers, and closes {@link LINGER} after nothing at or beneath its prefix is watched.
 * A narrower watch rides an open broader request, and a narrower request already open stays
 * open until the broader one covering it goes, so neither takes the other's routes over
 * before it has synced, which not every version can tell. A route both report is inserted once and
 * driven by its first reporter; if it changes hands anyway (a request failing), it restarts
 * unless one epoch proves both requests saw one instance.
 *
 * A peer that refuses overlapping requests gets exclusive discovery instead: a broader request
 * closes the narrower ones beneath it, and the session keeps it off the wire until they have
 * left. The routes a closed request carried go with it, and come back once the broader request
 * reports them.
 *
 * Each active announcement inserts a route covering the announced prefix, served lazily
 * through this connection, so nothing touches the wire until somebody requests a path
 * under it. A retraction removes the entry, and so does the session dying, which is what
 * scopes remote routes to the session that announced them. A reconnect wires a fresh
 * session to the same origin and re-opens whatever is still watched.
 *
 * A peer that refuses or resets an announce request downgrades the whole session to blind
 * answers: plain requests subscribe through it without waiting for an announcement, and
 * later watches open nothing on it.
 *
 * @internal
 */
export function forwardAnnounced(conn: Established, origin: OriginProducer): void {
	const originWire = wireOf(origin);
	const exclusive = wireOf(conn).exclusive;
	// Reassigned on a downgrade, so the origin stops counting this session as discovering.
	// Called through a closure so the session's death always detaches the current one.
	let detach = originWire.attach(true);
	let dead = false;
	let blind = false;

	const interests = new Map<Path.Valid, Interest>();
	const routes = new Map<Path.Valid, Reported>();

	void serveRequests(conn, origin);

	const report = (interest: Interest, event: Announce.Event) => {
		const { prefix, route } = event;
		if (event.kind === "end") {
			unreport(interest, prefix);
			return;
		}
		interest.reported.add(prefix);

		const existing = routes.get(prefix);
		if (!existing) {
			const handle = originWire.receive(prefix, route);
			routes.set(prefix, { handle, reporters: new Map([[interest, route]]) });
			void drive(handle, conn);
			return;
		}

		const joined = existing.reporters.has(interest);
		existing.reporters.set(interest, route);
		// Another request covering the prefix only holds it; the first reporter drives it.
		if (joined && existing.reporters.keys().next().value === interest) {
			follow(existing, prefix, route, event.kind === "restart");
		}
	};

	/**
	 * Bring the handle up to its driving reporter's route: a fresh route for another publisher
	 * instance, so nothing resolved through the old one is joined again, and an in-place
	 * update otherwise.
	 */
	const follow = (existing: Reported, prefix: Path.Valid, route: Route, restart: boolean) => {
		if (restart || route.epoch !== existing.handle.route.epoch) {
			// Inserted before the old one leaves, so local readers see one restart rather than
			// an end and a start.
			const handle = originWire.receive(prefix, route);
			existing.handle.close();
			existing.handle = handle;
			void drive(handle, conn);
		} else if (!routesEqual(existing.handle.route, route)) {
			existing.handle.updateRoute(route);
		}
	};

	const unreport = (interest: Interest, prefix: Path.Valid) => {
		interest.reported.delete(prefix);
		const existing = routes.get(prefix);
		if (!existing) return;
		const driving = existing.reporters.keys().next().value === interest;
		if (!existing.reporters.delete(interest)) return;

		const next = existing.reporters.values().next();
		if (next.done) {
			routes.delete(prefix);
			existing.handle.close();
		} else if (driving) {
			// The next request takes over driving. Without an epoch nothing proves it reports the
			// instance the handle serves, and the session consumes each request's announcements as
			// an instance of their own, so the route restarts rather than stitch two together.
			follow(existing, prefix, next.value, next.value.epoch === undefined);
		}
	};

	const downgrade = (failure: unknown) => {
		if (blind) return;
		blind = true;
		// Plain requests can still use this session's blind answers. Announcement followers
		// keep waiting for a source that can discover their path.
		console.warn("broadcast discovery failed; broadcasts resolve on request only.", failure);
		detach();
		detach = originWire.attach(false);
	};

	const open = (prefix: Path.Valid) => {
		if (exclusive) {
			for (const [other, narrower] of interests) {
				if (other === prefix || !Path.hasPrefix(prefix, other)) continue;
				interests.delete(other);
				narrower.closing = true;
				narrower.linger?.();
				narrower.linger = undefined;
				narrower.announced.close();
			}
		}

		// Hidden and covering routes are mirrored too; each local reader opts in on its own.
		const interest: Interest = {
			announced: conn.announced(Path.Pattern.subtree(prefix), { hidden: true, covering: true }),
			reported: new Set(),
			closing: false,
			held: new Map(),
		};
		interests.set(prefix, interest);

		// The origin's readers under the prefix wait on this request's initial set. Released
		// once the session stream has synced, which it reports only after the last of those
		// events is reported here. A closing stream also reads as synced, but a failure is not
		// an answer: the loop below releases it with the downgrade, in one step.
		const snapshot = originWire.snapshot(prefix);
		const synced = interest.announced.synced.subscribe((synced) => {
			if (synced && interest.announced.closed.peek() === undefined) snapshot();
		});
		// A request with no initial set to wait for is synced before the listener exists.
		if (interest.announced.synced.peek()) snapshot();

		void (async () => {
			let failure: unknown;
			try {
				for (;;) {
					const event = await interest.announced.next();
					if (!event) break;
					if (originWire.accepts(event.prefix)) report(interest, event);
				}
			} catch (err) {
				// The session died mid-stream, or the peer refused or reset the request.
				failure = err;
			} finally {
				synced();
				snapshot();
				for (const reported of [...interest.reported]) unreport(interest, reported);

				interest.announced.close();
				interest.linger?.();
				if (interests.get(prefix) === interest) interests.delete(prefix);
				if (!dead && !interest.closing) downgrade(failure);
				// What this request held now settles on its own, unless it is watched again.
				for (const [held, narrower] of interest.held) {
					if (!dead && interests.get(held) === narrower && !beneath.has(held)) release(held, narrower);
				}
			}
		})();
	};

	// How many reports want each prefix; overlapping watches can name one prefix twice.
	const wanted = new Map<Path.Valid, number>();
	// How many wanted prefixes sit at or beneath each prefix, so a request knows in a walk up
	// its path whether anything beneath it is still watched.
	const beneath = new Map<Path.Valid, number>();

	/**
	 * Settle a request nothing beneath it watches any more. A broader open request that is
	 * still needed holds it until that one goes, so the broader never takes its routes over
	 * before it has synced; otherwise it lingers out.
	 */
	const release = (prefix: Path.Valid, interest: Interest) => {
		for (const above of coveringPrefixes(prefix)) {
			if (above === prefix) continue;
			const holder = interests.get(above);
			if (!holder || !beneath.has(above)) continue;
			holder.held.set(prefix, interest);
			return;
		}
		interest.linger?.();
		interest.linger = after(Nano.fromMilli(LINGER), () => {
			// Forgotten now rather than once its stream winds down, so a watch arriving
			// meanwhile opens a fresh request instead of reviving this one.
			interests.delete(prefix);
			interest.closing = true;
			interest.announced.close();
		});
	};

	const unsubscribe = originWire.interests((prefix, want) => {
		if (dead) return;
		const count = (wanted.get(prefix) ?? 0) + (want ? 1 : -1);
		if (count > 0) wanted.set(prefix, count);
		else wanted.delete(prefix);
		if (count !== (want ? 1 : 0)) return;

		for (const above of coveringPrefixes(prefix)) {
			const held = (beneath.get(above) ?? 0) + (want ? 1 : -1);
			if (held > 0) beneath.set(above, held);
			else beneath.delete(above);
		}

		// A request stays open while anything beneath it is watched, so a narrower watch rides
		// it rather than opening a request that would later have to take its routes over.
		let covered = false;
		for (const above of coveringPrefixes(prefix)) {
			const interest = interests.get(above);
			if (!interest) continue;
			covered = true;
			if (beneath.has(above)) {
				interest.linger?.();
				interest.linger = undefined;
			} else {
				release(above, interest);
			}
		}
		if (want && !covered && !blind) open(prefix);
	});

	void conn.closed.then(() => {
		dead = true;
		unsubscribe();
		// End every request the moment the session closes rather than waiting for the wire to
		// error it, so the retractions land promptly.
		for (const interest of interests.values()) interest.announced.close();
		detach();
	});
}

/** Answer the requests under a received route by consuming them from the session. */
async function drive(handle: Requests, conn: Established): Promise<void> {
	const session = wireOf(conn);
	try {
		for await (const request of handle.requested()) {
			hooks.acceptRequest(request, session.consume(request.path));
		}
	} catch {
		handle.close();
	}
}

/**
 * Answer the origin's open requests with blind subscriptions for the session's lifetime.
 *
 * Every session answers, discovery or not: subscribing to an unannounced path is always
 * legal, and a missing broadcast surfaces as a reset on the first track. The first session
 * to answer wins; when this session dies its answers are withdrawn, and the requests they
 * resolved end with it, since a later session is another publisher instance.
 *
 * A path the table already routes is left alone. A request resolves to the table's route over
 * any blind answer, so answering one would only park a handle nothing reads.
 */
async function serveRequests(conn: Established, origin: OriginProducer): Promise<void> {
	const table = wireOf(origin);
	const session = wireOf(conn);
	// The withdraws for the answers this session provided, so a dead session only takes
	// back its own. Keyed by path but remembering the slot, because a path outlives its
	// slot: the last handle closing tears the slot down and a new request installs a fresh
	// one, and those two writes coalesce into a single wakeup. Matching on the path alone
	// would read the new slot as already answered and leave it unanswered forever.
	const answered = new Map<Path.Valid, { slot: RequestSlot; withdraw: Dispose }>();

	let dead = false;
	const closed = conn.closed.then(() => {
		dead = true;
	});

	for (;;) {
		const map = table.requests.peek();
		if (!map || dead) break;

		for (const [path, slot] of map) {
			if (!table.blind(slot)) continue;
			if (answered.get(path)?.slot === slot || slot.answer !== undefined) continue;
			if (table.routes(path)) continue;
			const withdraw = table.answer(path, session.consume(path));
			if (withdraw) answered.set(path, { slot, withdraw });
		}

		// A withdrawn request already released the answer; just forget our claim on the path.
		// A replaced slot counts as withdrawn: the answer we hold belongs to the slot that
		// went away, not to whatever now occupies the path.
		for (const [path, entry] of [...answered]) {
			if (map.get(path) === entry.slot && table.blind(entry.slot)) continue;
			answered.delete(path);
			entry.withdraw();
		}

		// Woken by the table too, not just the requests: a path that stops being routed needs
		// the blind answer this loop skipped while it was.
		await race([table.changed(), closed]);
	}

	// Session gone: withdraw our answers, waking a standby session to provide fresh ones.
	for (const { withdraw } of answered.values()) {
		withdraw();
	}
	answered.clear();
}
