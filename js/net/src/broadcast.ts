/**
 * Broadcast role handles: a named collection of tracks produced by a publisher.
 *
 * @module
 */
import { type GetPromise, Once, Signal } from "@moq/signals";
import type * as Epoch from "./epoch.ts";
import { NotFound, StreamCode, StreamError } from "./error.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import type { Route } from "./hop.ts";
import { hooks } from "./internal.ts";
import { Lister, Listing, listTracks, type Tracks } from "./listing.ts";
import * as Path from "./path.ts";
import * as track from "./track.ts";
import { untilAborted } from "./util/abort.ts";
import { registerWire, trackOf } from "./wire.ts";

export { Tracks, type TracksEvent } from "./listing.ts";

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

// Info lookups pending or held on a track.
class Lookups {
	pending = 0;
	// Whether a lookup opened the request, so the last lookup to end closes it unless subscribed.
	opened = false;
}

/** Reactive backing state shared by broadcast producers and consumers. */
class BroadcastState {
	requested = new Signal<track.Request[]>([]);
	pending = new Set<track.Request>();
	closed = new Once<null>();
	// The tracks the broadcast lists, and the readers listing them.
	listing = new Listing(this.closed);
	// The tracks the application inserted. A finished one stays and keeps serving its cache; only an
	// abort or removeTrack drops it, the way only a dropped producer leaves a Rust broadcast.
	tracks = new Map<string, track.Producer>();
	// The on-demand producer for each name, shared by every subscription and info lookup until it closes.
	requests = new Map<string, track.Producer>();
	lookups = new WeakMap<track.Producer, Lookups>();
	// Whether something answers on-demand track requests: a wire layer's consumed broadcast, or a
	// publisher that has started pulling them. The counterpart of a live Rust `broadcast::Requests`;
	// without one, a track nobody publishes is `NotFound` rather than a request nobody will answer.
	served = false;
	// Live consumer handles sharing this state (see {@link Consumer.clone}).
	consumers = 0;
	// A wire layer's broadcast, which has no producer: it ends once no consumer holds it, as a
	// Rust session retires a source nothing holds. A produced broadcast ends only on its close.
	unheld = false;
	// Run synchronously once the broadcast closes, before `closed` notifies its observers.
	closers = new Set<() => void>();
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
	state.listing.close();
	for (const request of state.pending)
		request.reject(
			new StreamError(StreamCode.Internal, {
				message: `track request ${request.name} dropped before acceptance`,
			}),
		);
	state.requested.mutate((requests) => {
		requests.length = 0;
	});
	for (const fn of state.closers) fn();
	state.closers.clear();
}

// The producer already serving `name`, evicting entries that can no longer serve it.
function lookup(state: BroadcastState, name: string): track.Producer | undefined {
	const inserted = state.tracks.get(name);
	if (inserted) {
		if (!(hooks.trackReleased(inserted).peek() instanceof Error)) return inserted;
		state.tracks.delete(name);
	}

	const requested = state.requests.get(name);
	if (requested) {
		if (hooks.trackReleased(requested).peek() === undefined) return requested;
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
// the entry so a later subscribe requests the track again.
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
		producer.abort(new NotFound(`track ${name}`));
		return { producer, requested: false };
	}

	const lookups = new Lookups();
	state.lookups.set(producer, lookups);
	state.requests.set(name, producer);
	void hooks.trackReleased(producer).then(() => {
		if (state.requests.get(name) === producer) state.requests.delete(name);
	});

	state.requested.mutate((requested) => {
		requested.push(hooks.makeRequest({ name, producer, pending: state.pending }));
	});

	return { producer, requested: true };
}

function subscribe(state: BroadcastState, name: string, options?: track.Subscription): track.Subscriber {
	return logical(state, name).producer.subscribe(options);
}

// A lookup is held until `hold` aborts, which also abandons it before the answer. A request a lookup opened is let go once it is answered and nobody holds or subscribes
// to it; closing it sooner would hand its handler a closed producer.
async function resolveTrackInfo(state: BroadcastState, name: string, hold?: AbortSignal): Promise<track.Info> {
	hold?.throwIfAborted();
	const { producer, requested } = logical(state, name);
	const lookups = state.lookups.get(producer);
	if (!lookups) return producer.info();
	if (requested) lookups.opened = true;
	lookups.pending++;

	const info = producer.info();
	let answered = false;
	let ended = false;
	const release = () => {
		if (lookups.opened && lookups.pending === 0 && producer.demand().wanted.peek() === undefined)
			hooks.releaseTrack(producer);
	};
	info.then(
		() => {
			answered = true;
			if (ended) release();
		},
		() => {},
	);
	const end = () => {
		ended = true;
		lookups.pending--;
		if (answered) release();
	};

	if (!hold) {
		try {
			return await info;
		} finally {
			end();
		}
	}
	hold.addEventListener("abort", end, { once: true });
	return untilAborted(info, hold);
}

// The newest frame cached for `name`, without subscribing: undefined when nobody serves it yet.
function largest(state: BroadcastState, name: string): track.Position | undefined {
	const producer = lookup(state, name);
	return producer && hooks.cachedLargest(producer);
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
	const subscriber = subscribe(state, name, { ...track.Subscription.default(), priority: options.priority });
	hooks.fetchBudget(subscriber, options.maxDelay);
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

// Remove and return the request queued for `name`, if no handler has taken it yet.
function takeQueued(state: BroadcastState, name: string): track.Request | undefined {
	const queued = state.requested.peek().find((request) => request.name === name);
	if (!queued) return undefined;
	state.requested.mutate((requests) => {
		requests.splice(requests.indexOf(queued), 1);
	});
	return queued;
}

// Serve `producer` as the broadcast's track for its name, listed with `info`.
function adopt(state: BroadcastState, producer: track.Producer, info: track.Info): void {
	const lookups = state.lookups.get(producer) ?? new Lookups();
	state.lookups.set(producer, lookups);
	state.tracks.set(producer.name, producer);
	state.listing.listOwn(producer.name, producer, info);

	// A finished track keeps serving its cache, so only an abort evicts it.
	void hooks.trackReleased(producer).then((closed) => {
		if (!(closed instanceof Error)) return;
		if (state.tracks.get(producer.name) === producer) state.tracks.delete(producer.name);
		state.listing.unlistOwn(producer.name, producer);
	});
}

/**
 * The write side of a broadcast.
 *
 * @public
 */
export class Producer {
	#state = new BroadcastState();
	#announcer?: Announcer;
	#path = Path.empty();

	constructor() {
		registerWire(this, {
			subscribe: (name, options) => subscribe(this.#state, name, options),
			resolveTrackInfo: (name, hold) => resolveTrackInfo(this.#state, name, hold),
			largest: (name) => largest(this.#state, name),
			fetchGroup: (name, sequence, options) => fetchGroup(this.#state, name, sequence, options),
			requested: () => requested(this.#state),
			tracks: (owner) => listTracks(this.#state.listing, owner),
			lister: () => new Lister(this.#state.listing),
		});
	}

	static {
		attachAnnouncer = (producer, announcer) => {
			producer.#announcer = announcer;
		};
		hooks.attachAnnouncer = attachAnnouncer;

		hooks.onClosed = (producer, fn) => {
			const closers = producer.#state.closers;
			closers.add(fn);
			return () => closers.delete(fn);
		};
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

	/** A read handle for this broadcast, named by the path the origin created it at. */
	consume(): Consumer {
		return makeConsumer({ state: this.#state, path: this.#path });
	}

	/**
	 * Create a track, insert it into the broadcast, and return its producer.
	 *
	 * A request still queued for the name is answered with this track, so its subscribers read
	 * from the returned producer. A request a handler has already taken stays a duplicate.
	 * An append on the track throws if it would reuse a sequence an earlier track of the name
	 * wrote in this broadcast, so a replacement writes its first group explicitly. Throws if an
	 * open track or a live request already holds the name.
	 */
	createTrack(name: string, info: Partial<track.Info> = {}): track.Producer {
		if (this.#state.closed.peek() !== undefined) {
			throw new Error("broadcast is closed");
		}

		const request = takeQueued(this.#state, name);
		if (request) {
			const producer = request.accept(info);
			// The application owns it now: a lookup that opened the request must not close it.
			const lookups = this.#state.lookups.get(producer);
			if (lookups) lookups.opened = false;
			this.#state.requests.delete(name);
			adopt(this.#state, producer, track.infoDefaults(info));
			return producer;
		}

		// One logical track per name: an open track or a live request already serves it.
		// A request a handler already pulled stays duplicate, as in Rust.
		const live = [this.#state.tracks.get(name), this.#state.requests.get(name)];
		if (live.some((existing) => existing && hooks.trackReleased(existing).peek() === undefined)) {
			throw new Error(`duplicate track: ${name}`);
		}

		const producer = new track.Producer(name).accept(info);
		adopt(this.#state, producer, track.infoDefaults(info));
		return producer;
	}

	/**
	 * Remove a statically inserted track, including a finished cached one, and stop listing it
	 * at once.
	 */
	removeTrack(name: string): void {
		const track = this.#state.tracks.get(name);
		if (!track) return;
		this.#state.tracks.delete(name);
		this.#state.listing.unlistOwn(name, track);
	}

	/** A lazy read handle for a track on this broadcast. */
	track(name: string): track.Consumer {
		return trackOf(name, this);
	}

	/** End the broadcast for good: retract its route and serve no new tracks. Idempotent. */
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
	unlisted?: boolean;
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
	// Served on request through a route claiming a shorter prefix, which names no broadcast to list.
	#unlisted: boolean;

	// Guards against a double close() on this handle over-decrementing the consumer count.
	#closed = false;

	protected constructor(shared?: never);
	protected constructor(shared?: Shared) {
		this.#path = shared?.path ?? Path.empty();
		this.#epoch = shared?.epoch;
		this.#unlisted = shared?.unlisted ?? false;
		if (shared) {
			this.#state = shared.state;
		} else {
			// A standalone consumer is a wire layer's, which serves every track on demand.
			this.#state = new BroadcastState();
			this.#state.served = true;
			this.#state.unheld = true;
		}
		this.#state.consumers++;
		registerWire(this, {
			subscribe: (name, options) => subscribe(this.#state, name, options),
			resolveTrackInfo: (name, hold) => resolveTrackInfo(this.#state, name, hold),
			largest: (name) => largest(this.#state, name),
			fetchGroup: (name, sequence, options) => fetchGroup(this.#state, name, sequence, options),
			requested: () => requested(this.#state),
			tracks: (owner) => this.#tracks(owner),
			lister: () => new Lister(this.#state.listing),
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
		hooks.stampUnlisted = (target) => {
			target.#unlisted = true;
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
	 * Shared by every {@link clone}. A produced broadcast settles once its producer closes; a
	 * broadcast consumed from a session, once the session ends it or the last handle closes.
	 * The subscribing wire layer peeks it to evict a closed entry from its per-path consume cache.
	 */
	get closed(): GetPromise<null> {
		return this.#state.closed;
	}

	/**
	 * Return another handle to the same broadcast, reference-counted with this one.
	 *
	 * Both handles read the same tracks, carry the same {@link path}, and share one {@link closed}
	 * state. Used by the connection's per-path
	 * consume cache to share one subscription across callers. Subclasses that resolve info over
	 * the wire override this to preserve their type (see the wire layer's consumed broadcast).
	 */
	clone(): Consumer {
		return new Consumer(this.shareState());
	}

	/**
	 * This consumer's backing state and path, for a clone. Opaque (`never`) so the state type
	 * stays unexported; a subclass passes it straight back into its own `super(...)`.
	 */
	protected shareState(): never {
		return {
			state: this.#state,
			path: this.#path,
			epoch: this.#epoch,
			unlisted: this.#unlisted,
		} satisfies Shared as never;
	}

	/** Get a lazy handle for a track on this broadcast. Repeat subscriptions dedupe onto one upstream subscription. */
	track(name: string): track.Consumer {
		return trackOf(name, this);
	}

	/**
	 * List the broadcast's tracks: each one with its info, then `synced`, then changes as they
	 * happen, until the broadcast ends.
	 *
	 * Lists the tracks its producer created (until they abort or are removed) and, for a
	 * broadcast consumed from a session, the tracks the peer lists. Through a session the
	 * listing syncs at once, with what this side holds, and the peer's tracks follow as live
	 * adds; every listing of one broadcast shares one request to the peer. A track served only
	 * on request is never listed, and listing never requests one. Throws once the broadcast
	 * has closed.
	 */
	tracks(): Tracks {
		return this.#tracks();
	}

	#tracks(owner?: object): Tracks {
		if (this.#unlisted) throw new NotFound("a route claiming a prefix lists no tracks");
		return listTracks(this.#state.listing, owner);
	}

	/**
	 * Release this handle, like dropping a Rust handle. A produced broadcast stays open until
	 * its producer closes; one consumed from a session ends once no {@link clone} holds it.
	 */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		if (--this.#state.consumers > 0 || !this.#state.unheld) return;
		closeState(this.#state);
	}
}
