/**
 * Broadcast role handles: a named collection of tracks produced by a publisher.
 *
 * @module
 */
import { type Dispose, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import { NotFound } from "./error.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import { Route } from "./hop.ts";
import { hooks, type TrackSequence } from "./internal.ts";
import * as Path from "./path.ts";
import * as track from "./track.ts";
import { untilAborted } from "./util/abort.ts";
import { registerWire, trackOf, type Broadcast as Wire } from "./wire.ts";

/** The origin callback a created broadcast uses to advertise its exact path. @internal */
export interface Announcer {
	/** Advertise or re-price this broadcast's path. */
	announce(route: Route): void;
	/** Retract the advertisement from local consumers and peers alike. */
	unannounce(): void;
}

let attachAnnouncer: (producer: Producer, announcer: Announcer) => void;
let stampProducer: (producer: Producer, path: Path.Valid) => void;

/** Reactive backing state shared by broadcast producers and consumers. */
class BroadcastState {
	requested = new Signal<track.Request[]>([]);
	pending = new Set<track.Request>();
	closed = new Once<null>();
	tracks = new Map<string, track.Producer>();
	sequences = new Map<string, TrackSequence>();
	// Live consumer handles sharing this state (see {@link Consumer.clone}). The broadcast
	// closes once the last one closes, so a shared consumer can be handed to several callers.
	consumers = 0;
	used = new Signal(false);
	active = 0;
	watched = new WeakSet<track.Producer>();
	demandCleanup = new Set<Dispose>();
}

// Each track updates the aggregate on an edge, so a demand change touches only its track.
function watchDemand(state: BroadcastState, producer: track.Producer): void {
	if (state.watched.has(producer)) return;
	state.watched.add(producer);
	const demand = producer.demand();
	let active = false;
	const update = () => {
		const used = state.closed.peek() === undefined && demand.closed.peek() === undefined && demand.used.peek();
		if (active === used) return;
		state.active += used ? 1 : -1;
		active = used;
		state.used.set(state.active > 0);
	};
	const disposeUsed = demand.used.subscribe(update);
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
		state.demandCleanup.delete(cleanup);
	};
	state.demandCleanup.add(cleanup);
	update();
}

function dequeueRequest(state: BroadcastState): track.Request | undefined {
	const requested = state.requested.peek();
	requested.sort((a, b) => a.priority - b.priority);
	return requested.pop();
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
	for (const cleanup of state.demandCleanup) cleanup();
	for (const request of state.pending) request.reject();
	state.requested.mutate((requests) => {
		requests.length = 0;
	});
}

// `register` is set on the subscribing (consumer) side: the fresh producer is cached in
// `state.tracks` so repeat subscriptions to the same track fan out from one upstream subscription
// instead of opening a new one, mirroring the Rust `broadcast::Consumer::track` weak-dedup. The
// consumer wire watches the producer's demand ({@link track.Demand.used}) and tears the upstream
// down once its last subscriber leaves, closing the producer, which evicts the cache entry below.
// The publishing side leaves `register` false: `state.tracks` there holds only the tracks the app
// inserted, and a dynamic serve stays one request per peer subscription.
function subscribe(
	state: BroadcastState,
	name: string,
	options: track.Subscription = {},
	register = false,
): track.Subscriber {
	if (state.closed.peek() !== undefined) {
		throw new Error("broadcast is closed");
	}

	const existing = state.tracks.get(name);
	if (existing) {
		if (existing.closed.peek() === undefined) return existing.subscribe(options);
		state.tracks.delete(name);
	}

	const producer = new track.Producer(name);
	watchDemand(state, producer);
	const subscriber = producer.subscribe(options);

	if (register) {
		state.tracks.set(name, producer);
		// Drop the cache entry once the upstream closes (the wire tears it down when its last
		// subscriber leaves), so a later subscribe re-opens it.
		void producer.closed.then(() => {
			if (state.tracks.get(name) === producer) state.tracks.delete(name);
		});
	}

	state.requested.mutate((requested) => {
		requested.push(hooks.makeRequest({ name, producer, sequences: state.sequences, pending: state.pending }));
	});

	return subscriber;
}

async function resolveTrackInfo(state: BroadcastState, name: string): Promise<track.Info> {
	const existing = state.tracks.get(name);
	if (existing && existing.closed.peek() === undefined) {
		return existing.info();
	}

	if (state.closed.peek() !== undefined) {
		return Promise.reject(new Error("broadcast is closed"));
	}

	const producer = new track.Producer(name);
	watchDemand(state, producer);
	state.requested.mutate((requested) => {
		requested.push(hooks.makeRequest({ name, producer, sequences: state.sequences, pending: state.pending }));
	});

	try {
		return await producer.info();
	} finally {
		producer.close();
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

	/** Whether any track currently has subscribers. */
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
		registerWire(this, this.#wire(false));
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

	async #requested(): Promise<track.Request | undefined> {
		for (;;) {
			const request = dequeueRequest(this.#state);
			if (request) return request;

			if (this.#state.closed.peek() !== undefined) return undefined;

			await Signal.race(this.#state.requested, this.#state.closed);
		}
	}

	/** Insert a track that is served directly, without an on-demand request round-trip. */
	insertTrack(track: track.Producer): void {
		if (this.#state.closed.peek() !== undefined) {
			throw new Error("broadcast is closed");
		}

		const existing = this.#state.tracks.get(track.name);
		if (existing && existing.closed.peek() === undefined) {
			throw new Error(`duplicate track: ${track.name}`);
		}

		watchDemand(this.#state, track);
		this.#state.tracks.set(track.name, track);

		void track.closed.then(() => {
			if (this.#state.tracks.get(track.name) === track) {
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

	/** Remove a statically inserted track by name. */
	removeTrack(name: string): void {
		this.#state.tracks.delete(name);
	}

	/** A lazy read handle for a track on this broadcast. */
	track(name: string): track.Consumer {
		return trackOf(name, this);
	}

	#wire(register: boolean): Wire {
		return {
			subscribe: (name, options) => subscribe(this.#state, name, options, register),
			resolveTrackInfo: (name) => resolveTrackInfo(this.#state, name),
			fetchGroup: (name, sequence, options) => fetchGroup(this.#state, name, sequence, options),
			requested: () => this.#requested(),
		};
	}

	/**
	 * Advertise this broadcast's exact path, or re-price a standing advertisement in place.
	 *
	 * Call it once the tracks a subscriber needs first (a catalog) exist. Until then the
	 * broadcast exists for nobody, on its own origin or at a peer. Retracts on
	 * {@link unannounce} or {@link close}. Throws if this producer was not created through an
	 * origin, or if the broadcast is already closed.
	 */
	announce(route: Route | { hops?: Route["hops"]; cost?: Route["cost"] | bigint } = Route.default): void {
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

	// Guards against a double close() on this handle over-decrementing the consumer count.
	#closed = false;

	protected constructor(shared?: never);
	protected constructor(shared?: Shared) {
		this.#state = shared?.state ?? new BroadcastState();
		this.#path = shared?.path ?? Path.empty();
		this.#state.consumers++;
		registerWire(this, {
			subscribe: (name, options) => subscribe(this.#state, name, options, true),
			resolveTrackInfo: (name) => resolveTrackInfo(this.#state, name),
			fetchGroup: (name, sequence, options) => fetchGroup(this.#state, name, sequence, options),
			requested: () => this.#requested(),
		});
	}

	static {
		makeConsumer = (shared) => new Consumer(shared as never);
		hooks.stampPath = (target, path) => {
			if (target instanceof Consumer) target.#path = path;
			else stampProducer(target, path);
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
		return { state: this.#state, path: this.#path } satisfies Shared as never;
	}

	/** Get a lazy handle for a track on this broadcast. Repeat subscriptions dedupe onto one upstream subscription. */
	track(name: string): track.Consumer {
		return trackOf(name, this);
	}

	async #requested(): Promise<track.Request | undefined> {
		for (;;) {
			const request = dequeueRequest(this.#state);
			if (request) return request;

			if (this.#state.closed.peek() !== undefined) return undefined;

			await Signal.race(this.#state.requested, this.#state.closed);
		}
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
