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
import { Derived, type Dispose, type GetPromise, type Getter, getter, Once, Signal } from "@moq/signals";
import * as announce from "./announced.ts";
import * as broadcast from "./broadcast.ts";
import * as Path from "./path.ts";

/**
 * One requested path: the notify node for everything watching it.
 *
 * `route` is the only reactive part, and the only thing a {@link Request} subscribes to, so
 * a publish or retraction anywhere else in the table cannot wake it. The origin's tables stay
 * the storage; this is a per-path view onto them, refreshed by whichever mutator touched the
 * path. The alternative, deriving each request over the whole `local`/`remote` maps, wakes
 * every open request on every unrelated change.
 *
 * `answer` outlives a session: the answering session clears it when it dies and the next one
 * answers again, which is what makes a request span reconnects.
 *
 * @internal
 */
export interface RequestSlot {
	count: number;
	answer?: broadcast.Consumer;
	readonly route: Signal<broadcast.Consumer | undefined>;
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

	// How many things are prepared to answer a request: attached sessions, plus reconnecting
	// connections that have no session right now but will. Zero means an unrouted path is
	// unroutable rather than merely unanswered, which is the whole difference between "wait,
	// this is coming" and "nothing here can ever serve you".
	answerers = new Signal(0);

	closed = new Once<Error | null>();

	/**
	 * Recompute what `path` resolves to, waking only the requests watching that path.
	 *
	 * A no-op for a path nobody requested, so the common case (publishing into a table
	 * nobody is asking about) costs a map lookup. Call after any write that could change
	 * the answer for a single path.
	 */
	refresh(path: Path.Valid): void {
		const slot = this.requests.peek()?.get(path);
		if (!slot) return;
		slot.route.set(this.route(path, slot.answer));
	}

	/** What `path` resolves to: the table's route when it has one, else the blind answer. */
	route(path: Path.Valid, answer?: broadcast.Consumer): broadcast.Consumer | undefined {
		return this.local.peek()?.get(path) ?? this.remote.peek()?.get(path)?.[0] ?? answer;
	}
}

/**
 * A non-owning handle on an origin: publish into it and read it, without its lifecycle.
 *
 * What a shared connection lends out. {@link Producer} implements it, so code that is
 * handed an origin rather than owning one should accept this type: closing the origin
 * stays the owner's alone, and a borrower cannot express it.
 *
 * @public
 */
export interface Table {
	/** Settles once the origin closes; see {@link Producer.closed}. */
	readonly closed: GetPromise<Error | null>;

	/** Whether every attached session announces into the table; see {@link Consumer.discovery}. */
	readonly discovery: Getter<boolean | undefined>;

	/** Publish a broadcast at `path`, returning its producer; see {@link Producer.publish}. */
	publish(path: Path.Valid): broadcast.Producer;

	/** Resolve `path`, without waiting for an announcement; see {@link Consumer.request}. */
	request(path: Path.Valid): Request;

	/** The available broadcasts under `prefix`, as a live stream; see {@link Consumer.announced}. */
	announced(prefix?: Path.Valid): announce.Consumer;
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
export class Producer implements Table {
	#state = new OriginState();

	// The reader backing the passthroughs, so holding a Producer never requires the
	// consume().x() stutter for everyday reads. One instance, so `discovery` keeps its
	// identity across reads.
	#reader = makeConsumer(this.#state);

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
		this.#state.refresh(path);

		// Unpublish when the broadcast closes, unless a republish already replaced it: a
		// stale broadcast closing must not unpublish the live one.
		void front.closed.then(() => {
			this.#state.local.mutate((broadcasts) => {
				if (broadcasts?.get(path) === front) broadcasts.delete(path);
			});
			this.#state.refresh(path);
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
		this.#state.refresh(path);

		return () => {
			this.#state.remote.mutate((broadcasts) => {
				const fronts = broadcasts?.get(path);
				if (!fronts) return;
				const index = fronts.indexOf(front);
				if (index < 0) return;
				fronts.splice(index, 1);
				if (fronts.length === 0) broadcasts?.delete(path);
			});
			this.#state.refresh(path);
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
		const release = this.expect();
		let detached = false;
		return () => {
			if (detached) return;
			detached = true;
			this.#sessions(-1, discovery);
			release();
		};
	}

	#sessions(delta: number, discovery: boolean): void {
		this.#state.sessions.update(({ total, discovery: d }) => ({
			total: total + delta,
			discovery: d + (discovery ? delta : 0),
		}));
	}

	/**
	 * Declare that something will answer requests on this origin, even with no session
	 * attached right now.
	 *
	 * A reconnecting connection holds one for its whole life, so a request made during a
	 * reconnect (or before the first session establishes) stays pending instead of reading as
	 * unroutable. Without it, {@link Request.unroutable} would fire on every page load, in the
	 * window between wiring the origin up and the handshake completing. Call the returned
	 * dispose when the connection is done for good.
	 *
	 * @internal
	 */
	expect(): Dispose {
		this.#state.answerers.update((count) => count + 1);
		let released = false;
		return () => {
			if (released) return;
			released = true;
			// Clamped because closing the origin zeroes the count, and the sessions attached at
			// the time still release afterwards.
			this.#state.answerers.update((count) => Math.max(0, count - 1));
		};
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
	 * Resolves once anything a serving session scans changes: the open requests, or either
	 * side of the routing table.
	 *
	 * @internal
	 */
	changed(): Promise<unknown> {
		return Signal.race(this.#state.requests, this.#state.local, this.#state.remote);
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
		if (!slot || slot.answer !== undefined) {
			front.close();
			return undefined;
		}
		slot.answer = front;
		this.#state.refresh(path);

		return () => {
			if (slot.answer === front) {
				slot.answer = undefined;
				this.#state.refresh(path);
				// The route signal only reaches this path's requesters; poke the map so every
				// serving loop re-scans and one of them re-answers.
				this.#state.requests.mutate(() => {});
			}
			front.close();
		};
	}

	/** A read handle for this origin, the side a connection's `publish` option borrows. */
	consume(): Consumer {
		return makeConsumer(this.#state);
	}

	/** Whether every attached session announces into the table; see {@link Consumer.discovery}. */
	get discovery(): Getter<boolean | undefined> {
		return this.#reader.discovery;
	}

	/** Resolve `path`, without waiting for an announcement; see {@link Consumer.request}. */
	request(path: Path.Valid): Request {
		return this.#reader.request(path);
	}

	/** Whether the table routes `path` itself; see {@link Consumer.routes}. @internal */
	routes(path: Path.Valid): boolean {
		return this.#reader.routes(path);
	}

	/** The available broadcasts under `prefix`, as a live stream; see {@link Consumer.announced}. */
	announced(prefix?: Path.Valid): announce.Consumer {
		return this.#reader.announced(prefix);
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
		// Nothing will answer a request on a closed origin, whatever is still attached, so
		// existing requests report unroutable rather than waiting on a corpse.
		this.#state.answerers.set(0);
		this.#state.requests.update((requests) => {
			for (const slot of requests?.values() ?? []) {
				slot.answer?.close();
				slot.answer = undefined;
				slot.route.set(undefined);
			}
			return undefined;
		});
	}
}

// Constructs a Consumer from within this module without exposing a public constructor
// that would leak the unexported OriginState. Assigned in the class's static block.
let makeConsumer: (state: OriginState) => Consumer;

// Same for Request: a public constructor would let a caller forge a handle that no origin
// ever registered, whose lifecycle guarantees are then false. `@internal` alone would not
// stop it, since the declaration emit keeps the constructor.
let makeRequest: (
	path: Path.Valid,
	active: Getter<broadcast.Consumer | undefined>,
	unroutable: Getter<boolean>,
	dispose: Dispose,
) => Request;

/**
 * An open request for a path nothing announced; see {@link Consumer.request}.
 *
 * @public
 */
export class Request {
	/** The requested path. */
	readonly path: Path.Valid;

	/**
	 * The resolved broadcast, or undefined while nothing provides the path.
	 *
	 * The table's route when it has one: a local publish (no round trip) or an announced
	 * broadcast, swapping when a republish takes the path. Otherwise a session's blind
	 * answer, which is assumed present rather than known live: a missing broadcast
	 * surfaces as a reset on the first track subscription, not here. Drops back to
	 * undefined when the providing route dies and resolves again when another appears.
	 *
	 * Yours for as long as the request is open: it is a handle of this request's own, so
	 * closing it ends your view of the path rather than the route everyone else reads.
	 * {@link close} releases whatever is current.
	 */
	readonly active: Getter<broadcast.Consumer | undefined>;

	/**
	 * Whether nothing can serve this path, as opposed to not having served it yet.
	 *
	 * True when the origin routes nothing here and nothing is prepared to answer: no session
	 * attached and no connection reconnecting toward one. False whenever {@link active} is
	 * set, and false while a connection is still coming up, so the ordinary page-load window
	 * before the first handshake reads as pending rather than as a missing broadcast. Waiting
	 * on this is futile by definition; wait for an announcement instead, via the origin's
	 * `announced`.
	 */
	readonly unroutable: Getter<boolean>;

	#dispose: Dispose;
	#closed = false;

	private constructor(
		path: Path.Valid,
		active: Getter<broadcast.Consumer | undefined>,
		unroutable: Getter<boolean>,
		dispose: Dispose,
	) {
		this.path = path;
		this.active = active;
		this.unroutable = unroutable;
		this.#dispose = dispose;
	}

	static {
		makeRequest = (path, active, unroutable, dispose) => new Request(path, active, unroutable, dispose);
	}

	/** Withdraw the request. The path stays routed for any other open request. Idempotent. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#dispose();
	}
}

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
		// True only when every attached session announces. One session that cannot means the
		// table is an incomplete picture, so a consumer gated on it has to keep its blind
		// fallback: the paths only that session carries never reach the table at all.
		this.#discovery = new Derived([state.sessions], ({ total, discovery }) =>
			total === 0 ? undefined : discovery === total,
		);
	}

	static {
		makeConsumer = (state) => new Consumer(state);
	}

	/** Settles once the origin closes; see {@link Producer.closed}. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/**
	 * Whether the announcement table sees everything the attached sessions can serve.
	 *
	 * Undefined while no session is attached (nothing is known yet), true when every attached
	 * session announces into the table, and false as soon as one does not, where
	 * {@link announced} cannot be complete and consumers should {@link request} paths instead
	 * of waiting. One blind session among several is still false: the paths only it carries
	 * never reach the table, so a consumer that trusted the gate would never see them.
	 */
	get discovery(): Getter<boolean | undefined> {
		return this.#discovery;
	}

	// Derived per access rather than cached: a lightweight mapped view over the session
	// counts, avoiding a Computed's lifecycle.
	readonly #discovery: Getter<boolean | undefined>;

	/**
	 * Whether the table routes `path` itself, by a local publish or a session's announcement.
	 *
	 * Availability, not a handle: {@link request} is the only way to consume by path. A
	 * request on a routed path resolves to that route and never to a blind answer, which is
	 * why a serving session leaves it alone.
	 *
	 * @internal
	 */
	routes(path: Path.Valid): boolean {
		if (this.#state.local.peek()?.has(path)) return true;
		return (this.#state.remote.peek()?.get(path)?.length ?? 0) > 0;
	}

	/**
	 * Resolve `path`, without waiting for an announcement.
	 *
	 * The one way to consume by path. {@link Request.active} follows whatever the table
	 * routes (a local publish, or any feeding session's announcement, swapping on a
	 * republish); when nothing does, the request stands and whichever attached session
	 * answers first provides a blind subscription instead, re-answered across reconnects.
	 * Close the request when done. On a closed origin it never resolves.
	 *
	 * With several sessions on one origin the first to answer wins, and it may be one that
	 * does not carry the path. Nothing corrects that: a missing broadcast surfaces as a reset
	 * on the first track and deliberately leaves the handle open, since the wire cannot tell
	 * "not here" from "not yet" and a blind handle is expected to survive until a publisher
	 * arrives. It matters only on an origin mixing sessions that announce with sessions that
	 * cannot, where a path only the silent session carries may sit behind another session's
	 * answer. Prefer {@link unroutable} and announcements over blind requests when the origin
	 * feeds from more than one connection.
	 */
	request(path: Path.Valid): Request {
		const requests = this.#state.requests.peek();
		if (!requests) {
			// Closed origin: a request that can never resolve, and says so.
			return makeRequest(path, new Signal<broadcast.Consumer | undefined>(undefined), getter(true), () => {});
		}

		let slot = requests.get(path);
		if (!slot) {
			// Seeded through the constructor, so a path the table already routes resolves on the
			// first read. It must not go through a silent set: that still captures the pre-seed
			// value as the baseline the next change is compared against, and never flushes to
			// clear it, so a seeded route retracting to undefined would look like no change and
			// notify nobody.
			const created: RequestSlot = { count: 0, route: new Signal(this.#state.route(path)) };
			slot = created;
			this.#state.requests.mutate((map) => {
				map?.set(path, created);
			});
		}
		slot.count += 1;

		// Hand out a handle of the request's own rather than the table's. Closing a consumer
		// closes the broadcast once it was the last one, and the table often holds the only
		// other handle, so lending its front out means an ordinary close() by one requester
		// can unpublish the path for everybody else.
		const taken = slot;

		// Memoized on the route's identity: the same front resolving again returns the handle
		// we already made, and only a real swap clones a new one (cloning before closing the
		// old, so a broadcast that both routes share never briefly loses its last handle).
		let released = false;
		let source: broadcast.Consumer | undefined;
		let handle: broadcast.Consumer | undefined;
		const own = (front: broadcast.Consumer | undefined): broadcast.Consumer | undefined => {
			if (released) return undefined;
			if (front !== source) {
				const previous = handle;
				source = front;
				handle = front?.clone();
				previous?.close();
			}
			return handle;
		};

		const route = taken.route;
		const active = new Derived([route], own);

		// Swapping on the read is what keeps a routed path resolving synchronously, but a
		// holder that only ever peeked would then pin a route that has already been retracted
		// until it happened to read again. Following the route as well retires it promptly,
		// and the memo makes the two paths agree: whichever runs first does the swap.
		const unsubscribe = route.subscribe(own);

		// Only meaningful while nothing is routed, so it reads the route rather than `active`:
		// the two cannot disagree, since a routed path always has an answerer-independent
		// answer.
		const unroutable = new Derived([route, this.#state.answerers], (front, answerers) => !front && answerers === 0);

		return makeRequest(path, active, unroutable, () => {
			// Releases this request's handle; the route itself belongs to the table.
			released = true;
			unsubscribe();
			handle?.close();
			handle = undefined;
			source = undefined;

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
				taken.answer?.close();
				taken.answer = undefined;
				taken.route.set(undefined);
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
