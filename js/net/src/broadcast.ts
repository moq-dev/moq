import { Signal } from "@moq/signals";
import type { Consumer as GroupConsumer } from "./group.ts";
import * as track from "./track.ts";

/** Reactive backing state shared by broadcast producers and consumers. */
class BroadcastState {
	requested = new Signal<track.Request[]>([]);
	closed = new Signal<boolean | Error>(false);
	tracks = new Map<string, track.Producer>();
}

function closedPromise(state: BroadcastState): Promise<Error | undefined> {
	return new Promise((resolve) => {
		const dispose = state.closed.subscribe((closed) => {
			if (!closed) return;
			resolve(closed instanceof Error ? closed : undefined);
			dispose();
		});
	});
}

// Close the broadcast and reject any requests still pending in the queue, so a
// subscriber blocked on the track's info() or group reads is unblocked rather
// than left waiting on a producer that will never be served.
function closeState(state: BroadcastState, abort?: Error) {
	state.closed.set(abort ?? true);
	state.requested.mutate((requests) => {
		for (const request of requests) request.reject(abort);
		requests.length = 0;
	});
}

function subscribe(state: BroadcastState, name: string, priority: number): track.Subscriber {
	if (state.closed.peek()) {
		throw new Error(`broadcast is closed: ${state.closed.peek()}`);
	}

	const existing = state.tracks.get(name);
	if (existing) {
		if (!existing.closedSignal.peek()) return existing.subscribe();
		state.tracks.delete(name);
	}

	const producer = new track.Producer(name);
	const subscriber = producer.subscribe();
	state.requested.mutate((requested) => {
		requested.push(new track.Request(name, producer, priority));
		requested.sort((a, b) => a.priority - b.priority);
	});

	return subscriber;
}

async function resolveTrackInfo(state: BroadcastState, name: string): Promise<track.Info> {
	const existing = state.tracks.get(name);
	if (existing && !existing.closedSignal.peek()) {
		return existing.info();
	}

	if (state.closed.peek()) {
		return Promise.reject(new Error(`broadcast is closed: ${state.closed.peek()}`));
	}

	const producer = new track.Producer(name);
	state.requested.mutate((requested) => {
		requested.push(new track.Request(name, producer, 0));
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
	const subscriber = subscribe(state, name, options.priority ?? 0);
	try {
		for (;;) {
			const group = await subscriber.recvGroup();
			if (!group) throw new Error(`group not found: ${sequence}`);
			if (group.sequence === sequence) return group;

			group.close();
			if (group.sequence > sequence) throw new Error(`group not found: ${sequence}`);
		}
	} finally {
		subscriber.close();
	}
}

/**
 * The write side of a broadcast.
 *
 * @public
 */
export class Producer implements track.Broadcast {
	#state = new BroadcastState();

	/** Resolves with the abort error (or undefined) once closed. */
	readonly closed: Promise<Error | undefined>;

	constructor() {
		this.closed = closedPromise(this.#state);
	}

	/** A read handle for this broadcast. */
	consume(): Consumer {
		return new Consumer(this.#state as never);
	}

	/** Return the next track requested by a peer. */
	async requested(): Promise<track.Request | undefined> {
		for (;;) {
			const request = this.#state.requested.peek().pop();
			if (request) return request;

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

			await Signal.race(this.#state.requested, this.#state.closed);
		}
	}

	/** Insert a track that is served directly, without an on-demand request round-trip. */
	insertTrack(track: track.Producer): void {
		if (this.#state.closed.peek()) {
			throw new Error(`broadcast is closed: ${this.#state.closed.peek()}`);
		}

		const existing = this.#state.tracks.get(track.name);
		if (existing && !existing.closedSignal.peek()) {
			throw new Error(`duplicate track: ${track.name}`);
		}

		this.#state.tracks.set(track.name, track);

		void track.closed.finally(() => {
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
	subscribe(name: string, priority: number): track.Subscriber {
		return subscribe(this.#state, name, priority);
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

	/** Close the broadcast, optionally with an error to abort waiters. */
	close(abort?: Error) {
		closeState(this.#state, abort);
	}
}

/**
 * The read side of a broadcast.
 *
 * @public
 */
export class Consumer implements track.Broadcast {
	#state: BroadcastState;

	/** Resolves with the abort error (or undefined) once closed. */
	readonly closed: Promise<Error | undefined>;

	constructor(state?: never);
	constructor(state?: BroadcastState) {
		this.#state = state ?? new BroadcastState();
		this.closed = closedPromise(this.#state);
	}

	/** Get a lazy handle for a track on this broadcast. */
	track(name: string): track.Consumer {
		return new track.Consumer(name, this);
	}

	/** Open a live subscription to a track. Used by the subscribing wire layer. */
	subscribe(name: string, priority: number): track.Subscriber {
		return subscribe(this.#state, name, priority);
	}

	/** Return the next track requested by the local consumer. Used by the subscribing wire layer. */
	async requested(): Promise<track.Request | undefined> {
		for (;;) {
			const request = this.#state.requested.peek().pop();
			if (request) return request;

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed) return undefined;

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

	/** Close the broadcast, optionally with an error to abort waiters. */
	close(abort?: Error) {
		closeState(this.#state, abort);
	}
}
