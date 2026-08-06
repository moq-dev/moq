/**
 * A broadcast routing table, independent of any connection.
 *
 * Publish broadcasts into an origin and hand the origin to one or more connections to
 * serve them; the broadcasts outlive any single session. Hand the same (or another)
 * origin to a connection's `subscribe` option and the peer's announced broadcasts appear
 * in the table too, consumable by path. Mirrors the `origin` module in `rs/moq-net`.
 *
 * @module
 */
import { type Dispose, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import * as announce from "./announced.ts";
import * as broadcast from "./broadcast.ts";
import * as Path from "./path.ts";

/**
 * One requested path: how many {@link Request} handles want it, and the front the first
 * session to answer provided. The front signal outlives a session: the answering session
 * clears it when it dies, and the next session answers again, which is what makes a
 * request span reconnects.
 *
 * @internal
 */
export interface RequestSlot {
	count: number;
	front: Signal<broadcast.Consumer | undefined>;
}

/** Reactive backing state shared by origin producers and consumers. */
class OriginState {
	// Both tables hold consumer fronts, so the application producing into the origin and
	// the connections serving or feeding it stay decoupled. Undefined once the origin
	// closes, so late writes fail loudly.
	//
	// Local is what this endpoint publishes; sessions announce and serve it. Remote is
	// what sessions feeding the origin discovered; an entry dies with the session that
	// inserted it. They are separate maps so a session can never announce a remote entry
	// back to a peer, which is what makes an origin shared by both directions echo-free.
	//
	// Remote keeps every session's front per path, newest first: [0] is the route consumers
	// resolve, and removing it promotes the next, so a session dying does not black-hole a
	// path another live session still carries (which would stay dark forever, since that
	// session already announced it and will not again).
	local = new Signal<Map<Path.Valid, broadcast.Consumer> | undefined>(new Map());
	remote = new Signal<Map<Path.Valid, broadcast.Consumer[]> | undefined>(new Map());

	// Paths consumers asked for without waiting for an announcement; attached sessions
	// answer them with blind subscriptions. Never announced: an answered request is assumed
	// present, not known live, so it must not read as an availability claim.
	requests = new Signal<Map<Path.Valid, RequestSlot> | undefined>(new Map());

	// How many sessions are attached, and how many of those support broadcast discovery.
	// What backs the public `discovery` getter.
	sessions = new Signal({ total: 0, discovery: 0 });

	closed = new Once<Error | null>();
}

/**
 * The write side of an origin: publish broadcasts by path.
 *
 * Independent of any connection. A connection given this origin (via its `publish` option)
 * announces and serves the table's broadcasts for as long as the session lasts; the
 * broadcasts themselves live until their producer closes or {@link close} tears the origin
 * down. A reconnecting session re-announces the table on each attach, so publishes made
 * while offline surface on the next connection.
 *
 * @public
 */
export class Producer {
	#state = new OriginState();

	/**
	 * Settles once the origin closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/**
	 * Publish a broadcast at `path`, returning its producer.
	 *
	 * Close the producer to unpublish. Publishing a path again supersedes the previous
	 * broadcast: the origin drops its handle on the old one, which closes it unless the
	 * application still holds a consumer clone. A local publish also shadows any remote
	 * broadcast at the same path.
	 */
	publish(path: Path.Valid): broadcast.Producer {
		const producer = new broadcast.Producer();
		const front = producer.consume();

		this.#state.local.mutate((broadcasts) => {
			if (!broadcasts) throw new Error("origin is closed");
			broadcasts.get(path)?.close();
			broadcasts.set(path, front);
		});

		// Unpublish when the broadcast closes, unless a republish already replaced it: a
		// stale broadcast closing must not unpublish the live one.
		void front.closed.then(() => {
			this.#state.local.mutate((broadcasts) => {
				if (broadcasts?.get(path) === front) broadcasts.delete(path);
			});
		});

		return producer;
	}

	/**
	 * Insert a broadcast discovered by a session, taking ownership of `front`.
	 *
	 * The newest insertion becomes the route consumers resolve; earlier ones are kept as
	 * fallbacks and promoted when it goes away, so overlapping sessions carrying the same
	 * path fail over instead of black-holing it. The returned dispose retracts this front
	 * (whichever position it holds) and releases it; call it when the announcement ends or
	 * the session dies. Inserting into a closed origin releases the front immediately and
	 * retracts nothing.
	 *
	 * @internal
	 */
	insertRemote(path: Path.Valid, front: broadcast.Consumer): Dispose {
		let closed = false;
		this.#state.remote.mutate((broadcasts) => {
			if (!broadcasts) {
				closed = true;
				return;
			}
			const fronts = broadcasts.get(path);
			if (fronts) fronts.unshift(front);
			else broadcasts.set(path, [front]);
		});
		if (closed) {
			front.close();
			return () => {};
		}

		return () => {
			this.#state.remote.mutate((broadcasts) => {
				const fronts = broadcasts?.get(path);
				if (!fronts) return;
				const index = fronts.indexOf(front);
				if (index < 0) return;
				fronts.splice(index, 1);
				if (fronts.length === 0) broadcasts?.delete(path);
			});
			front.close();
		};
	}

	/**
	 * Register an attached session, counting it toward the `discovery` state. Returns the
	 * detach; call it exactly once when the session dies.
	 *
	 * @internal
	 */
	attach(discovery: boolean): Dispose {
		this.#sessions(1, discovery);
		let detached = false;
		return () => {
			if (detached) return;
			detached = true;
			this.#sessions(-1, discovery);
		};
	}

	#sessions(delta: number, discovery: boolean): void {
		this.#state.sessions.update(({ total, discovery: d }) => ({
			total: total + delta,
			discovery: d + (discovery ? delta : 0),
		}));
	}

	/**
	 * The open requests, watched by attached sessions to answer them; see
	 * {@link Consumer.request}. Undefined once the origin closes.
	 *
	 * @internal
	 */
	get requests(): Getter<ReadonlyMap<Path.Valid, RequestSlot> | undefined> {
		return this.#state.requests;
	}

	/**
	 * Provide `front` as the answer for the open request on `path`, taking ownership of it.
	 *
	 * Returns undefined (releasing the front) when the request is gone or already answered;
	 * first session in wins, and a loser must stay eligible to answer later. The returned
	 * withdraw releases the front and, if it was the standing answer, vacates the slot and
	 * wakes the other serving loops so a standby session answers immediately; call it when
	 * the session dies.
	 *
	 * @internal
	 */
	answer(path: Path.Valid, front: broadcast.Consumer): Dispose | undefined {
		const slot = this.#state.requests.peek()?.get(path);
		if (!slot || slot.front.peek() !== undefined) {
			front.close();
			return undefined;
		}
		slot.front.set(front);

		return () => {
			if (slot.front.peek() === front) {
				slot.front.set(undefined);
				// The slot signal only reaches its requesters; poke the map so every
				// serving loop re-scans and one of them re-answers.
				this.#state.requests.mutate(() => {});
			}
			front.close();
		};
	}

	/** A read handle for this origin. */
	consume(): Consumer {
		return makeConsumer(this.#state);
	}

	/** Close the origin, every broadcast it still routes, and its announcement streams. Idempotent. */
	close(abort?: Error) {
		if (this.#state.closed.peek() !== undefined) return;
		this.#state.closed.set(abort ?? null);
		this.#state.local.update((broadcasts) => {
			for (const front of broadcasts?.values() ?? []) {
				front.close(abort);
			}
			return undefined;
		});
		this.#state.remote.update((broadcasts) => {
			// Remote broadcasts are somebody else's; only release our handles on them.
			for (const fronts of broadcasts?.values() ?? []) {
				for (const front of fronts) front.close();
			}
			return undefined;
		});
		this.#state.requests.update((requests) => {
			for (const slot of requests?.values() ?? []) {
				slot.front.peek()?.close();
				slot.front.set(undefined);
			}
			return undefined;
		});
	}
}

/**
 * An open request for a path nothing announced; see {@link Consumer.request}.
 *
 * @public
 */
export class Request {
	/** The requested path. */
	readonly path: Path.Valid;

	/**
	 * The broadcast a session provided, or undefined while nobody has.
	 *
	 * Assumed present rather than known live: the session subscribed blind, so a missing
	 * broadcast surfaces as a reset on the first track subscription, not here. Drops back
	 * to undefined when the answering session dies and resolves again once another answers.
	 */
	readonly active: Getter<broadcast.Consumer | undefined>;

	#dispose: Dispose;
	#closed = false;

	/** @internal Created by {@link Consumer.request}. */
	constructor(path: Path.Valid, active: Getter<broadcast.Consumer | undefined>, dispose: Dispose) {
		this.path = path;
		this.active = active;
		this.#dispose = dispose;
	}

	/** Withdraw the request. The path stays routed for any other open request. Idempotent. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#dispose();
	}
}

// Constructs a Consumer from within this module without exposing a public constructor
// that would leak the unexported OriginState. Assigned in the class's static block.
let makeConsumer: (state: OriginState) => Consumer;

/**
 * The read side of an origin: resolve broadcasts by path and watch what is available.
 *
 * Obtain one from {@link Producer.consume}. Pass it to a connection's `publish` option to
 * serve the origin's local broadcasts to that peer; read it directly to consume anything
 * the origin routes, locally published or discovered by a session.
 *
 * @public
 */
export class Consumer {
	#state: OriginState;

	private constructor(state: OriginState) {
		this.#state = state;
	}

	static {
		makeConsumer = (state) => new Consumer(state);
	}

	/** Settles once the origin closes; see {@link Producer.closed}. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/**
	 * Whether an attached session supports broadcast discovery.
	 *
	 * Undefined while no session is attached (nothing is known yet), true when at least one
	 * attached session announces broadcasts into the table, false when every attached
	 * session lacks discovery, where {@link announced} stays silent and consumers should
	 * {@link request} paths instead of waiting.
	 */
	get discovery(): Getter<boolean | undefined> {
		return this.#discovery;
	}

	// Derived per access rather than cached: a lightweight mapped view over the session
	// counts, avoiding a Computed's lifecycle.
	readonly #discovery: Getter<boolean | undefined> = {
		peek: () => {
			const { total, discovery } = this.#state.sessions.peek();
			return total === 0 ? undefined : discovery > 0;
		},
		subscribe: (fn) =>
			this.#state.sessions.subscribe(({ total, discovery }) => fn(total === 0 ? undefined : discovery > 0)),
		changed: ((fn?: (value: boolean | undefined) => void) => {
			const map = ({ total, discovery }: { total: number; discovery: number }) =>
				total === 0 ? undefined : discovery > 0;
			if (fn) return this.#state.sessions.changed((value) => fn(map(value)));
			return this.#state.sessions.changed().then(map);
		}) as Getter<boolean | undefined>["changed"],
	};

	/**
	 * A handle to the broadcast at `path`, or undefined when nothing routes it.
	 *
	 * A local publish wins over a remote broadcast at the same path, so a publisher
	 * consuming its own path reads its own copy with no round trip. The handle is yours:
	 * close it when done. The broadcast stays routed for everyone else.
	 */
	consume(path: Path.Valid): broadcast.Consumer | undefined {
		const local = this.#state.local.peek()?.get(path);
		if (local) return local.clone();
		return this.#state.remote.peek()?.get(path)?.[0]?.clone();
	}

	/**
	 * Ask the attached sessions to provide `path` without waiting for an announcement.
	 *
	 * The escape hatch for a path nothing announces: a relay without discovery, or a
	 * subscribe-immediately consumer that accepts a reset when the path turns out absent.
	 * Whichever attached session answers first backs {@link Request.active}, blind; the
	 * request outlives sessions, so a reconnect re-answers it. Close the request when done.
	 * On a closed origin the request never resolves.
	 */
	request(path: Path.Valid): Request {
		const requests = this.#state.requests.peek();
		if (!requests) {
			// Closed origin: a request that can never resolve, mirroring consume's undefined.
			return new Request(path, new Signal<broadcast.Consumer | undefined>(undefined), () => {});
		}

		let slot = requests.get(path);
		if (!slot) {
			const created: RequestSlot = { count: 0, front: new Signal<broadcast.Consumer | undefined>(undefined) };
			slot = created;
			this.#state.requests.mutate((map) => {
				map?.set(path, created);
			});
		}
		slot.count += 1;

		const taken = slot;
		return new Request(path, taken.front, () => {
			taken.count -= 1;
			if (taken.count > 0) return;

			// Defer the teardown a microtask: an effect whose rerun was triggered by the
			// answer resolving closes its old request and takes a new one in the same tick,
			// and tearing down in between would drop the answer it is about to read.
			queueMicrotask(() => {
				if (taken.count > 0) return;
				this.#state.requests.mutate((map) => {
					if (map?.get(path) === taken) map.delete(path);
				});
				taken.front.peek()?.close();
				taken.front.set(undefined);
			});
		});
	}

	/**
	 * The available broadcasts under `prefix`, as a live stream: everything currently
	 * routed arrives first as `active`, then additions and removals as they happen. Paths
	 * are relative to `prefix`. The stream ends when the origin closes or the consumer is
	 * closed.
	 */
	announced(prefix: Path.Valid = Path.empty()): announce.Consumer {
		const producer = new announce.Producer(prefix);
		void this.#runAnnounced(producer, prefix);
		return producer.consume();
	}

	async #runAnnounced(producer: announce.Producer, prefix: Path.Valid): Promise<void> {
		// Keyed by suffix, valued by the routing front. Diffing identity rather than mere
		// presence means a republish (a new broadcast taking the path) emits a retraction
		// then a fresh announcement, so a consumer re-consumes instead of clinging to the
		// superseded broadcast.
		let active = new Map<Path.Valid, broadcast.Consumer>();

		try {
			for (;;) {
				const local = this.#state.local.peek();
				const remote = this.#state.remote.peek();
				if (local === undefined && remote === undefined) break;

				const next = new Map<Path.Valid, broadcast.Consumer>();
				// Remote first, so a local publish at the same path overwrites it: the
				// announcement points at whatever consume() would resolve.
				for (const [path, fronts] of remote ?? []) {
					const suffix = Path.stripPrefix(prefix, path);
					if (suffix !== null && fronts[0]) next.set(suffix, fronts[0]);
				}
				for (const [path, front] of local ?? []) {
					const suffix = Path.stripPrefix(prefix, path);
					if (suffix !== null) next.set(suffix, front);
				}

				for (const [path, front] of active) {
					if (next.get(path) !== front) producer.append({ path, active: false });
				}
				for (const [path, front] of next) {
					if (active.get(path) !== front) producer.append({ path, active: true });
				}
				active = next;

				await Signal.race(this.#state.local, this.#state.remote, producer.closed);
				if (producer.closed.peek() !== undefined) return;
			}
		} catch {
			// The reader closed between the check and an append; nothing left to do.
		}
		producer.close();
	}

	/**
	 * The local table, borrowed by the wire publishers to answer announces and subscribes.
	 *
	 * Deliberately excludes remote entries: a session never re-announces what a peer told
	 * it, so an origin wired to both directions of a connection cannot echo. Borrowed, not
	 * owned: do not close the fronts. Undefined once the origin closes.
	 *
	 * @internal
	 */
	get broadcasts(): Getter<ReadonlyMap<Path.Valid, broadcast.Consumer> | undefined> {
		return this.#state.local;
	}
}
