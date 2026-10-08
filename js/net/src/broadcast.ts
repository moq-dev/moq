/**
 * Broadcast role handles: a named collection of tracks produced by a publisher.
 *
 * @module
 */
import { type Dispose, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import type * as Epoch from "./epoch.ts";
import { NotFound } from "./error.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import { Route } from "./hop.ts";
import { hooks, type TrackSequence } from "./internal.ts";
import * as Path from "./path.ts";
import * as track from "./track.ts";
import { untilAborted } from "./util/abort.ts";
import { registerWire, trackOf } from "./wire.ts";

/** The origin callback a created broadcast uses to advertise its exact path. @internal */
export interface Announcer {
	/** Advertise or re-price this broadcast's path. */
	announce(route: Route): void;
	/** Retract the advertisement from local consumers and peers alike. */
	unannounce(): void;
	/** The route this broadcast's path is advertised with, if it is. */
	route(): Route | undefined;
}

let attachAnnouncer: (producer: Producer, announcer: Announcer) => void;
let stampProducer: (producer: Producer, path: Path.Valid) => void;

// Info lookups pending on a requested track. Each is demand, as a pending track request is in Rust,
// though nobody subscribes.
class Lookups {
	pending = 0;
	// Re-evaluates the track's demand at once; set by watchDemand.
	changed = () => {};

	add(delta: number): void {
		this.pending += delta;
		this.changed();
	}
}

/** Reactive backing state shared by broadcast producers and consumers. */
class BroadcastState {
	requested = new Signal<track.Request[]>([]);
	pending = new Set<track.Request>();
	closed = new Once<null>();
	// The tracks the application inserted. A finished one stays and keeps serving its cache; only an
	// abort or removeTrack drops it, the way only a dropped producer leaves a Rust broadcast.
	tracks = new Map<string, track.Producer>();
	// The on-demand producer for each name, shared by every subscription and info lookup until it closes.
	requests = new Map<string, track.Producer>();
	lookups = new WeakMap<track.Producer, Lookups>();
	// Whether something answers on-demand track requests: a wire layer's consumed broadcast, or a
	// publisher that has started pulling them. The counterpart of a live Rust `broadcast::Dynamic`;
	// without one, a track nobody publishes is `NotFound` rather than a request nobody will answer.
	served = false;
	sequences = new Map<string, TrackSequence>();
	// Live consumer handles sharing this state (see {@link Consumer.clone}). The broadcast
	// closes once the last one closes, so a shared consumer can be handed to several callers.
	consumers = 0;
	used = new Signal(false);
	active = 0;
	// The demand watcher of each open track, disposed when it closes or leaves the broadcast.
	demands = new Map<track.Producer, Dispose>();
}

// Each track updates the aggregate on an edge, so a demand change touches only its track.
// A track counts as demand while any `lookups` are pending, subscribed or not.
function watchDemand(state: BroadcastState, producer: track.Producer, lookups?: Lookups): void {
	if (state.demands.has(producer)) return;
	const demand = producer.demand();
	// A closed track is never demand, and its close already fired, so nothing would dispose a watcher.
	if (demand.closed.peek() !== undefined) return;
	let active = false;
	const update = () => {
		const used =
			state.closed.peek() === undefined &&
			demand.closed.peek() === undefined &&
			((lookups?.pending ?? 0) > 0 || demand.used.peek());
		if (active === used) return;
		state.active += used ? 1 : -1;
		active = used;
		state.used.set(state.active > 0);
	};
	const disposeUsed = demand.used.subscribe(update);
	if (lookups) lookups.changed = update;
	const disposeClosed = demand.closed.subscribe(() => {
		if (demand.closed.peek() === undefined) return;
		cleanup();
	});
	const cleanup = () => {
		disposeUsed();
		disposeClosed();
		if (active) {
			state.active--;
			active = false;
			state.used.set(state.active > 0);
		}
		state.demands.delete(producer);
	};
	state.demands.set(producer, cleanup);
	update();
}

function dequeueRequest(state: BroadcastState): track.Request | undefined {
	const requested = state.requested.peek();
	requested.sort((a, b) => a.priority - b.priority);
	return requested.pop();
}

// The next on-demand track request, or undefined once the broadcast closes.
async function requested(state: BroadcastState): Promise<track.Request | undefined> {
	// Pulling requests is what makes a broadcast serve tracks on demand, and it latches for the
	// broadcast's lifetime since JS has no drop to mark the handler gone. A subscribe before the first
	// pull is answered NotFound, so an on-demand publisher starts pulling before it publishes and keeps
	// pulling until the broadcast closes.
	state.served = true;
	for (;;) {
		const request = dequeueRequest(state);
		if (request) return request;

		if (state.closed.peek() !== undefined) return undefined;

		await Signal.race(state.requested, state.closed);
	}
}

// Close the broadcast and reject any requests still pending in the queue, so a
// subscriber blocked on the track's info() or group reads is unblocked rather
// than left waiting on a producer that will never be served.
//
// Once.set throws on a second settle, and the producer and each consumer handle close
// independently, so this has to be idempotent.
function closeState(state: BroadcastState) {
	if (state.closed.peek() !== undefined) return;
	state.closed.set(null);
	for (const cleanup of state.demands.values()) cleanup();
	for (const request of state.pending) request.reject();
	state.requested.mutate((requests) => {
		requests.length = 0;
	});
}

// The producer already serving `name`, evicting entries that can no longer serve it.
function lookup(state: BroadcastState, name: string): track.Producer | undefined {
	const inserted = state.tracks.get(name);
	if (inserted) {
		if (!(inserted.closed.peek() instanceof Error)) return inserted;
		state.tracks.delete(name);
	}

	const requested = state.requests.get(name);
	if (requested) {
		if (requested.closed.peek() === undefined) return requested;
		state.requests.delete(name);
	}

	return undefined;
}

// The producer serving `name`, or a new one with a request queued for it.
//
// A broadcast has one logical track per name, mirroring the Rust `broadcast::Consumer::track`
// weak-dedup: a requested producer is cached in `state.requests` so every subscription and info
// lookup fans out from one request instead of opening another. Whoever serves it closes it once
// unused (the consuming wire tears the upstream down when its last subscriber leaves), which evicts
// the entry so a later subscribe requests the track again, continuing its sequences.
function logical(state: BroadcastState, name: string): { producer: track.Producer; requested: boolean } {
	if (state.closed.peek() !== undefined) {
		throw new Error("broadcast is closed");
	}

	const existing = lookup(state, name);
	if (existing) return { producer: existing, requested: false };

	const producer = new track.Producer(name);
	if (!state.served) {
		// Answer through the track rather than throwing, so a peer's subscribe is refused with
		// the code instead of an unhandled error.
		producer.close(new NotFound(`track ${name}`));
		return { producer, requested: false };
	}

	const lookups = new Lookups();
	state.lookups.set(producer, lookups);
	watchDemand(state, producer, lookups);
	state.requests.set(name, producer);
	void producer.closed.then(() => {
		if (state.requests.get(name) === producer) state.requests.delete(name);
	});

	state.requested.mutate((requested) => {
		requested.push(hooks.makeRequest({ name, producer, sequences: state.sequences, pending: state.pending }));
	});

	return { producer, requested: true };
}

function subscribe(state: BroadcastState, name: string, options: track.Subscription = {}): track.Subscriber {
	return logical(state, name).producer.subscribe(options);
}

async function resolveTrackInfo(state: BroadcastState, name: string): Promise<track.Info> {
	const { producer, requested } = logical(state, name);
	const lookups = state.lookups.get(producer);
	lookups?.add(1);
	try {
		return await producer.info();
	} finally {
		lookups?.add(-1);
		// A finished lookup is no demand: let go of a request it opened that nobody subscribed to
		// meanwhile.
		if (requested && !producer.demand().used.peek()) producer.close();
	}
}

// Serve a group from the local retained window by subscribing and scanning to the
// requested sequence. The default for a produced broadcast; the consuming wire layer
// overrides it to fetch over the network (or to reject when the transport has no FETCH).
async function fetchGroup(
	state: BroadcastState,
	name: string,
	sequence: number,
	options: track.FetchGroupOptions = {},
): Promise<GroupConsumer> {
	options.signal?.throwIfAborted();
	const subscriber = subscribe(state, name, { priority: options.priority });
	hooks.exemptFetch(subscriber);
	try {
		for (;;) {
			const group = await untilAborted(subscriber.recvGroup(), options.signal);
			if (!group) throw new NotFound(`group ${sequence}`);
			if (group.sequence === sequence) {
				// Close the subscription when the returned group finishes, not now: an
				// in-progress group must keep receiving frames for its lifetime (mirrors
				// Rust poll_fetch). Also fires if the caller closes the group early.
				void group.closed.then(() => subscriber.close());
				return group;
			}

			group.close();
			if (group.sequence > sequence) throw new NotFound(`group ${sequence}`);
		}
	} catch (err) {
		subscriber.close();
		throw err;
	}
}

let makeDemand: (state: BroadcastState) => Demand;

/** A watch-only view of demand for any track in a broadcast. */
export class Demand {
	#state: BroadcastState;
	private constructor(state: BroadcastState) {
		this.#state = state;
	}
	static {
		makeDemand = (state) => new Demand(state);
	}

	/** Whether any track currently has subscribers, or a track info query is pending. */
	get used(): Getter<boolean> {
		return this.#state.used;
	}

	/** Wait until every track becomes unused or the broadcast closes. */
	async unused(): Promise<void> {
		while (this.#state.used.peek() && this.#state.closed.peek() === undefined) {
			await Signal.race(this.#state.used, this.#state.closed);
		}
	}

	/** The broadcast's clean close. */
	get closed(): GetPromise<null> {
		return this.#state.closed;
	}
}

/**
 * The write side of a broadcast.
 *
 * @public
 */
export class Producer {
	#state = new BroadcastState();
	#demand = makeDemand(this.#state);
	#announcer?: Announcer;
	#path = Path.empty();

	constructor() {
		registerWire(this, {
			subscribe: (name, options) => subscribe(this.#state, name, options),
			resolveTrackInfo: (name) => resolveTrackInfo(this.#state, name),
			fetchGroup: (name, sequence, options) => fetchGroup(this.#state, name, sequence, options),
			requested: () => requested(this.#state),
		});
	}

	static {
		attachAnnouncer = (producer, announcer) => {
			producer.#announcer = announcer;
		};
		hooks.attachAnnouncer = attachAnnouncer;
		stampProducer = (producer, path) => {
			producer.#path = path;
		};
	}

	/**
	 * Settles with `null` once the broadcast closes; a broadcast end carries no cause.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<null> {
		return this.#state.closed;
	}

	/** Watch demand for the broadcast's tracks. */
	demand(): Demand {
		return this.#demand;
	}

	/** A read handle for this broadcast, named by the path the origin created it at. */
	consume(): Consumer {
		return makeConsumer({ state: this.#state, path: this.#path });
	}

	/** Insert a track that is served directly, without an on-demand request round-trip. */
	insertTrack(track: track.Producer): void {
		if (this.#state.closed.peek() !== undefined) {
			throw new Error("broadcast is closed");
		}

		// One logical track per name: an open inserted track or a live request already serves it.
		const live = [this.#state.tracks.get(track.name), this.#state.requests.get(track.name)];
		if (live.some((existing) => existing && existing.closed.peek() === undefined)) {
			throw new Error(`duplicate track: ${track.name}`);
		}

		watchDemand(this.#state, track);
		this.#state.tracks.set(track.name, track);

		// A finished track keeps serving its cache, so only an abort evicts it.
		void track.closed.then((closed) => {
			if (closed instanceof Error && this.#state.tracks.get(track.name) === track) {
				this.#state.tracks.delete(track.name);
			}
		});
	}

	/** Create a track, insert it into the broadcast, and return its producer. */
	createTrack(name: string, info: Partial<track.Info> = {}): track.Producer {
		const producer = new track.Producer(name).accept(info);
		this.insertTrack(producer);
		return producer;
	}

	/** Remove a statically inserted track, including a finished cached one, and stop counting it toward {@link demand} at once. */
	removeTrack(name: string): void {
		const track = this.#state.tracks.get(name);
		if (!track) return;
		this.#state.tracks.delete(name);
		this.#state.demands.get(track)?.();
	}

	/** A lazy read handle for a track on this broadcast. */
	track(name: string): track.Consumer {
		return trackOf(name, this);
	}

	/** The route this broadcast is announced with, or undefined while it is not announced. */
	get route(): Route | undefined {
		return this.#announcer?.route();
	}

	/**
	 * Advertise this broadcast's exact path, or replace the standing advertisement's route.
	 *
	 * The route is taken as given, epoch included: a route with another epoch (or none)
	 * announces a new broadcast, so re-price from the current one,
	 * `announce({ ...broadcast.route, cost })`, to keep the instance.
	 *
	 * Call it once the tracks a subscriber needs first (a catalog) exist. Until then the
	 * broadcast exists for nobody, on its own origin or at a peer. Retracts on
	 * {@link unannounce} or {@link close}. Throws if this producer was not created through an
	 * origin, or if the broadcast is already closed.
	 */
	announce(
		route: Route | { epoch?: Route["epoch"]; hops?: Route["hops"]; cost?: Route["cost"] | bigint } = Route.default,
	): void {
		if (this.#state.closed.peek() !== undefined) {
			throw new Error("broadcast is closed");
		}
		if (!this.#announcer) throw new Error("broadcast is not attached to an origin");
		this.#announcer.announce(Route.normalize(route));
	}

	/**
	 * Retract the advertisement of this broadcast's path, if any, from local consumers and
	 * peers alike. {@link announce} brings it back.
	 */
	unannounce(): void {
		this.#announcer?.unannounce();
	}

	/** End the broadcast for good: retract it, serve no new tracks, and refuse a later {@link announce}. Idempotent. */
	close(): void {
		this.#announcer?.unannounce();
		this.#announcer = undefined;
		closeState(this.#state);
	}
}

// What a new consumer handle inherits: the shared broadcast plus the path naming it.
interface Shared {
	state: BroadcastState;
	path: Path.Valid;
	epoch?: Epoch.Valid;
}

// Constructs a Consumer from within this module without exposing a public constructor
// that would leak the unexported BroadcastState. Assigned in the class's static block.
let makeConsumer: (shared: Shared) => Consumer;

/**
 * The read side of a broadcast.
 *
 * Created internally: obtain one from {@link Producer.consume} or an origin request.
 * The wire layers subclass it to resolve tracks over the network.
 *
 * @public
 */
export class Consumer {
	#state: BroadcastState;
	#path: Path.Valid;
	#epoch?: Epoch.Valid;

	// Guards against a double close() on this handle over-decrementing the consumer count.
	#closed = false;

	protected constructor(shared?: never);
	protected constructor(shared?: Shared) {
		this.#path = shared?.path ?? Path.empty();
		this.#epoch = shared?.epoch;
		if (shared) {
			this.#state = shared.state;
		} else {
			// A standalone consumer is a wire layer's, which serves every track on demand.
			this.#state = new BroadcastState();
			this.#state.served = true;
		}
		this.#state.consumers++;
		registerWire(this, {
			subscribe: (name, options) => subscribe(this.#state, name, options),
			resolveTrackInfo: (name) => resolveTrackInfo(this.#state, name),
			fetchGroup: (name, sequence, options) => fetchGroup(this.#state, name, sequence, options),
			requested: () => requested(this.#state),
		});
	}

	static {
		makeConsumer = (shared) => new Consumer(shared as never);
		hooks.stampPath = (target, path) => {
			if (target instanceof Consumer) target.#path = path;
			else stampProducer(target, path);
		};
		hooks.stampEpoch = (target, epoch) => {
			target.#epoch = epoch;
		};
	}

	/**
	 * The path this handle names the broadcast by, which relative references in its catalog
	 * (hang's `broadcast` field) resolve against.
	 *
	 * An origin stamps each handle it hands out with the path it was requested at, relative to
	 * that origin handle's scope root, and a broadcast it created with the path it was created at.
	 * Empty for a standalone broadcast, which is then its own root: any `..` reference escapes.
	 */
	get path(): Path.Valid {
		return this.#path;
	}

	/**
	 * The publisher epoch of the route an origin resolved this handle through, if it has one.
	 *
	 * An origin stamps it with each request's result, so it names the publisher
	 * instance actually serving this handle even when the request named none. `undefined` for a
	 * route without an epoch or a handle not handed out by an origin.
	 */
	get epoch(): Epoch.Valid | undefined {
		return this.#epoch;
	}

	/**
	 * Settles with `null` once the broadcast closes; a broadcast end carries no cause.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 *
	 * Shared by every {@link clone}: it settles once the last handle closes. The subscribing
	 * wire layer peeks it to evict a closed entry from its per-path consume cache.
	 */
	get closed(): GetPromise<null> {
		return this.#state.closed;
	}

	/**
	 * Return another handle to the same broadcast, reference-counted with this one.
	 *
	 * Both handles read the same tracks, carry the same {@link path}, and share one {@link closed}
	 * state; the broadcast closes only once *every* handle has {@link close}d. Used by the connection's per-path
	 * consume cache to share one subscription across callers. Subclasses that resolve info over
	 * the wire override this to preserve their type (see the wire layer's consumed broadcast).
	 */
	clone(): Consumer {
		return new Consumer(this.shareState());
	}

	// Hand this consumer's backing state and path to a clone. Opaque (`never`) so the state type
	// stays unexported; a subclass passes it straight back into its own `super(...)`.
	protected shareState(): never {
		return { state: this.#state, path: this.#path, epoch: this.#epoch } satisfies Shared as never;
	}

	/** Get a lazy handle for a track on this broadcast. Repeat subscriptions dedupe onto one upstream subscription. */
	track(name: string): track.Consumer {
		return trackOf(name, this);
	}

	/**
	 * Release this handle. The broadcast is closed once this was the last live handle;
	 * while other {@link clone}s remain open it stays live.
	 */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		if (--this.#state.consumers > 0) return;
		closeState(this.#state);
	}
}
