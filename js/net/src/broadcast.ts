/**
 * Broadcast role handles: a named collection of tracks produced by a publisher.
 *
 * @module
 */
import { type GetPromise, Once, Signal } from "@moq/signals";
import type { Consumer as GroupConsumer } from "./group.ts";
import { hooks } from "./internal.ts";
import * as track from "./track.ts";

/** Reactive backing state shared by broadcast producers and consumers. */
class BroadcastState {
	requested = new Signal<track.Request[]>([]);
	closed = new Once<Error | null>();
	tracks = new Map<string, track.Producer>();
	// Live consumer handles sharing this state (see {@link Consumer.clone}). The broadcast
	// closes once the last one closes, so a shared consumer can be handed to several callers.
	consumers = 0;
}

// Close the broadcast and reject any requests still pending in the queue, so a
// subscriber blocked on the track's info() or group reads is unblocked rather
// than left waiting on a producer that will never be served.
//
// Once.set throws on a second settle, and the producer and each consumer handle close
// independently, so this has to be idempotent.
function closeState(state: BroadcastState, abort?: Error) {
	if (state.closed.peek() !== undefined) return;
	state.closed.set(abort ?? null);
	state.requested.mutate((requests) => {
		for (const request of requests) request.reject(abort);
		requests.length = 0;
	});
}

// `register` is set on the subscribing (consumer) side: the fresh producer is cached in
// `state.tracks` so repeat subscriptions to the same track fan out from one upstream
// subscription instead of opening a new one, mirroring the Rust `broadcast::Consumer::track`
// weak-dedup. The publishing side leaves it false: `state.tracks` there holds only the tracks
// the app inserted, and a dynamic serve stays one request per peer subscription.
function subscribe(
	state: BroadcastState,
	name: string,
	options: track.Subscription = {},
	register = false,
): track.Subscriber {
	if (state.closed.peek() !== undefined) {
		throw new Error(`broadcast is closed: ${state.closed.peek()}`);
	}

	const existing = state.tracks.get(name);
	if (existing) {
		if (existing.closed.peek() === undefined) return existing.subscribe();
		state.tracks.delete(name);
	}

	const producer = new track.Producer(name);
	const subscriber = producer.subscribe();

	if (register) {
		state.tracks.set(name, producer);
		// Drop the cache entry once the subscription closes, so a later subscribe re-opens it.
		void producer.closed.then(() => {
			if (state.tracks.get(name) === producer) state.tracks.delete(name);
		});
	}

	state.requested.mutate((requested) => {
		requested.push(hooks.makeRequest(name, producer, options.priority ?? 0));
		requested.sort((a, b) => a.priority - b.priority);
	});

	return subscriber;
}

async function resolveTrackInfo(state: BroadcastState, name: string): Promise<track.Info> {
	const existing = state.tracks.get(name);
	if (existing && existing.closed.peek() === undefined) {
		return existing.info();
	}

	if (state.closed.peek() !== undefined) {
		return Promise.reject(new Error(`broadcast is closed: ${state.closed.peek()}`));
	}

	const producer = new track.Producer(name);
	state.requested.mutate((requested) => {
		requested.push(hooks.makeRequest(name, producer, 0));
		requested.sort((a, b) => a.priority - b.priority);
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
	const subscriber = subscribe(state, name, { priority: options.priority });
	try {
		for (;;) {
			const group = await subscriber.recvGroup();
			if (!group) throw new Error(`group not found: ${sequence}`);
			if (group.sequence === sequence) {
				// Close the subscription when the returned group finishes, not now: an
				// in-progress group must keep receiving frames for its lifetime (mirrors
				// Rust poll_fetch). Also fires if the caller closes the group early.
				void group.closed.then(() => subscriber.close());
				return group;
			}

			group.close();
			if (group.sequence > sequence) throw new Error(`group not found: ${sequence}`);
		}
	} catch (err) {
		subscriber.close();
		throw err;
	}
}

/**
 * The write side of a broadcast.
 *
 * @public
 */
export class Producer implements track.Broadcast {
	#state = new BroadcastState();

	/**
	 * Settles once the broadcast closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/** A read handle for this broadcast. */
	consume(): Consumer {
		return makeConsumer(this.#state);
	}

	/** Return the next track requested by a peer. */
	async requested(): Promise<track.Request | undefined> {
		for (;;) {
			const request = this.#state.requested.peek().pop();
			if (request) return request;

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return undefined;

			await Signal.race(this.#state.requested, this.#state.closed);
		}
	}

	/** Insert a track that is served directly, without an on-demand request round-trip. */
	insertTrack(track: track.Producer): void {
		if (this.#state.closed.peek() !== undefined) {
			throw new Error(`broadcast is closed: ${this.#state.closed.peek()}`);
		}

		const existing = this.#state.tracks.get(track.name);
		if (existing && existing.closed.peek() === undefined) {
			throw new Error(`duplicate track: ${track.name}`);
		}

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

	/** Open a live subscription to a track. Used by the publishing wire layer. */
	subscribe(name: string, options?: track.Subscription): track.Subscriber {
		return subscribe(this.#state, name, options);
	}

	/** Resolve a track's immutable info. Used by the publishing wire layer. */
	resolveTrackInfo(name: string): Promise<track.Info> {
		return resolveTrackInfo(this.#state, name);
	}

	/** Fetch a single group from the local retained window. Used by track handles. */
	fetchGroup(name: string, sequence: number, options?: track.FetchGroupOptions): Promise<GroupConsumer> {
		return fetchGroup(this.#state, name, sequence, options);
	}

	/** A lazy read handle for a track on this broadcast. */
	track(name: string): track.Consumer {
		return new track.Consumer(name, this);
	}

	/** Close the broadcast, optionally with an error to abort waiters. Idempotent. */
	close(abort?: Error) {
		closeState(this.#state, abort);
	}
}

// Constructs a Consumer from within this module without exposing a public constructor
// that would leak the unexported BroadcastState. Assigned in the class's static block.
let makeConsumer: (state: BroadcastState) => Consumer;

/**
 * The read side of a broadcast.
 *
 * Created internally: obtain one from {@link Producer.consume} or the connection's
 * `consume(path)`. The wire layers subclass it to resolve tracks over the network.
 *
 * @public
 */
export class Consumer implements track.Broadcast {
	#state: BroadcastState;

	// Guards against a double close() on this handle over-decrementing the consumer count.
	#closed = false;

	protected constructor(state?: never);
	protected constructor(state?: BroadcastState) {
		this.#state = state ?? new BroadcastState();
		this.#state.consumers++;
	}

	static {
		makeConsumer = (state) => new Consumer(state as never);
	}

	/**
	 * Settles once the broadcast closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 *
	 * Shared by every {@link clone}: it settles once the last handle closes. The subscribing
	 * wire layer peeks it to evict a closed entry from its per-path consume cache.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/**
	 * Return another handle to the same broadcast, reference-counted with this one.
	 *
	 * Both handles read the same tracks and share one {@link closed} state; the broadcast
	 * closes only once *every* handle has {@link close}d. Used by the connection's per-path
	 * consume cache to share one subscription across callers. Subclasses that resolve info over
	 * the wire override this to preserve their type (see the wire layer's consumed broadcast).
	 */
	clone(): Consumer {
		return new Consumer(this.shareState());
	}

	// Hand this consumer's backing state to a clone. Opaque (`never`) so the state type stays
	// unexported; a subclass passes it straight back into its own `super(...)`.
	protected shareState(): never {
		return this.#state as never;
	}

	/** Get a lazy handle for a track on this broadcast. Repeat subscriptions dedupe onto one upstream subscription. */
	track(name: string): track.Consumer {
		return new track.Consumer(name, this);
	}

	/** Open a live subscription to a track. Used by the subscribing wire layer. Repeat subscriptions to the same track share one upstream subscription. */
	subscribe(name: string, options?: track.Subscription): track.Subscriber {
		return subscribe(this.#state, name, options, true);
	}

	/** Return the next track requested by the local consumer. Used by the subscribing wire layer. */
	async requested(): Promise<track.Request | undefined> {
		for (;;) {
			const request = this.#state.requested.peek().pop();
			if (request) return request;

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return undefined;

			await Signal.race(this.#state.requested, this.#state.closed);
		}
	}

	/**
	 * Resolve a track's immutable info. Used by track handles. This base resolves it from
	 * the local producers; the consuming wire layer overrides it to fetch over the wire.
	 */
	resolveTrackInfo(name: string): Promise<track.Info> {
		return resolveTrackInfo(this.#state, name);
	}

	/**
	 * Fetch a single group by sequence. Used by track handles. This base serves from the
	 * local retained window; the consuming wire layer overrides it to fetch over the wire
	 * (or to reject when the transport has no FETCH).
	 */
	fetchGroup(name: string, sequence: number, options?: track.FetchGroupOptions): Promise<GroupConsumer> {
		return fetchGroup(this.#state, name, sequence, options);
	}

	/**
	 * Release this handle. The broadcast is closed (optionally with an error to abort waiters)
	 * once this was the last live handle; while other {@link clone}s remain open it stays live.
	 */
	close(abort?: Error) {
		if (this.#closed) return;
		this.#closed = true;
		if (--this.#state.consumers > 0) return;
		closeState(this.#state, abort);
	}
}
