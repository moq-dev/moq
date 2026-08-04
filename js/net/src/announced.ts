/**
 * Broadcast announcement streams: which broadcast paths are available under a prefix.
 *
 * @module
 */
import { Effect, type GetPromise, type Getter, type GetterInit, getter, Once, Signal } from "@moq/signals";
import type * as broadcast from "./broadcast.js";
import type { Established } from "./connection/established.js";
import * as Path from "./path.js";

/**
 * The availability of a broadcast.
 *
 * @public
 */
export interface Event {
	/** Broadcast path relative to the prefix passed to `announced()`. */
	path: Path.Valid;
	/** True when the broadcast is available, false when it was removed. */
	active: boolean;
}

/** Reactive backing state shared by announcement producers and consumers. */
class AnnounceState {
	queue = new Signal<Event[]>([]);
	closed = new Once<Error | null>();
}

// Once.set throws on a second settle, and both ends of a stream can close independently.
function closeState(state: AnnounceState, abort?: Error) {
	if (state.closed.peek() !== undefined) return;
	state.closed.set(abort ?? null);
	state.queue.mutate((queue) => {
		queue.length = 0;
	});
}

/**
 * The write side of an announcement stream.
 *
 * @public
 */
export class Producer {
	/** Path prefix this stream is scoped to. */
	prefix: Path.Valid;

	#state = new AnnounceState();

	constructor(prefix = Path.empty()) {
		this.prefix = prefix;
	}

	/**
	 * Settles once the stream closes: `null` on a clean close, or the abort {@link Error}.
	 * Peek it synchronously (`undefined` while open), observe it reactively, or `await` it.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	/** A read handle for this announcement stream. */
	consume(): Consumer {
		return makeConsumer(this.prefix, this.#state);
	}

	/** Writes an announcement to the queue. */
	append(event: Event) {
		if (this.#state.closed.peek() !== undefined) throw new Error("announcements are closed");
		this.#state.queue.mutate((queue) => {
			queue.push(event);
		});
	}

	/** Closes the writer. Idempotent. */
	close(abort?: Error) {
		closeState(this.#state, abort);
	}
}

// Constructs a Consumer from within this module without exposing a public constructor
// that would leak the unexported AnnounceState. Assigned in the class's static block.
let makeConsumer: (prefix: Path.Valid, state: AnnounceState) => Consumer;

/**
 * The read side of an announcement stream.
 *
 * Created internally: obtain one from {@link Producer.consume} or the connection's
 * `announced(prefix)`.
 *
 * @public
 */
export class Consumer {
	/** Path prefix this stream is scoped to. */
	prefix: Path.Valid;

	#state: AnnounceState;

	private constructor(prefix: Path.Valid, state: AnnounceState) {
		this.prefix = prefix;
		this.#state = state;
	}

	/** Settles once the stream closes; see {@link Producer.closed}. */
	get closed(): GetPromise<Error | null> {
		return this.#state.closed;
	}

	static {
		makeConsumer = (prefix, state) => new Consumer(prefix, state);
	}

	/** Returns the next announcement. */
	async next(): Promise<Event | undefined> {
		for (;;) {
			const announce = this.#state.queue.peek().shift();
			if (announce) return announce;

			const closed = this.#state.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) return undefined;

			await Signal.race(this.#state.queue, this.#state.closed);
		}
	}

	/** Closes the reader. Idempotent. */
	close(abort?: Error) {
		closeState(this.#state, abort);
	}
}

// Connections already warned about missing broadcast discovery, so the fallback logs at most
// once per connection instead of once per watched path.
const warnedNoDiscovery = new WeakSet<Established>();

// Constructs a Broadcast without exposing a public constructor. The connection types are the
// documented entry point, and they live in other modules, so this is re-exported as
// {@link watchBroadcast} rather than assigned from within this one.
let makeBroadcast: (connection: GetterInit<Established | undefined>, path: Path.Valid) => Broadcast;

/**
 * Construct a {@link Broadcast}. Call `announcedBroadcast(path)` on a connection instead.
 *
 * @internal
 */
export function watchBroadcast(connection: GetterInit<Established | undefined>, path: Path.Valid): Broadcast {
	return makeBroadcast(connection, path);
}

/**
 * A reactive handle to a single broadcast: {@link Broadcast.active} holds a live
 * {@link broadcast.Consumer} while the path is announced and `undefined` while nobody
 * publishes it.
 *
 * Use this instead of {@link Established.consume} whenever the broadcast may not exist yet.
 * Subscribing to a path nobody publishes gets the stream reset, so a consumer that races the
 * publisher stays silent forever unless it retries; this waits for the announcement instead.
 *
 * The handle re-consumes on every (re-)announce, so a same-name republish (a new publisher, or
 * a relay-failover RESTART) re-attaches to the new instance rather than clinging to the dead
 * one. Built from a reconnecting `Connection.Reload`, it also spans reconnects: the
 * broadcast drops to `undefined` while disconnected and resolves again once the new connection
 * announces it.
 *
 * Falls back to consuming blind (and warns once) on a relay without
 * {@link Established.discovery}, where there is no announcement to wait for.
 *
 * If discovery fails on a live session (the announcement stream is reset, or the relay
 * refuses it) the handle goes offline and stays there: nothing reopens the stream on that
 * connection. Build it from a `Connection.Reload` if you need it to recover, since a new
 * connection starts a new stream.
 *
 * Close it to release the announcement stream and the current broadcast.
 *
 * @public
 */
export class Broadcast {
	/** The broadcast path this handle watches. */
	readonly path: Path.Valid;

	/** The live broadcast, or `undefined` while it is offline. */
	readonly active: Getter<broadcast.Consumer | undefined>;

	#active = new Signal<broadcast.Consumer | undefined>(undefined);
	#signals = new Effect();

	static {
		makeBroadcast = (connection, path) => new Broadcast(connection, path);
	}

	// Accepts a live Established session or a reactive one (a Reload's `established`), which is
	// how the handle survives reconnects.
	private constructor(connection: GetterInit<Established | undefined>, path: Path.Valid) {
		this.path = path;
		this.active = this.#active;

		const source = getter(connection);
		this.#signals.run((effect) => {
			const conn = effect.get(source);
			if (!conn) return;

			// Without discovery no announcement ever arrives, so waiting would hang forever.
			if (!conn.discovery) {
				if (!warnedNoDiscovery.has(conn)) {
					warnedNoDiscovery.add(conn);
					console.warn("relay does not support broadcast discovery; consuming without waiting.");
				}

				const blind = conn.consume(path);
				effect.cleanup(() => blind.close());
				effect.set(this.#active, blind, undefined);

				// Nothing will announce it back, but `active` still promises a *live* broadcast,
				// so stop advertising this one once the wire resets it.
				void blind.closed.then(() => {
					if (this.#active.peek() === blind) this.#active.set(undefined);
				});
				return;
			}

			const announced = conn.announced(path);
			effect.cleanup(() => announced.close());

			let current: broadcast.Consumer | undefined;
			const offline = () => {
				const mine = current;
				current?.close();
				current = undefined;
				// Only clear what this run put there. A spawn task that resumes after its run was
				// torn down would otherwise wipe the consumer a newer run already installed.
				if (this.#active.peek() === mine) this.#active.set(undefined);
			};
			effect.cleanup(offline);

			effect.spawn(async () => {
				try {
					for (;;) {
						const event = await Promise.race([effect.cancel, announced.next()]);
						if (!event) break;

						// Scoped to `path`, so the exact broadcast arrives with an empty suffix; ignore children.
						if (event.path !== Path.empty()) continue;

						if (event.active) {
							// A live subscription survives a redundant (re-)announce; only replace a dead one.
							if (current && current.closed.peek() === undefined) continue;
							current?.close();
							current = conn.consume(path);
							this.#active.set(current);
						} else {
							offline();
						}
					}
				} catch (err) {
					// Discovery failed: the session died under the stream, or the relay refused
					// to answer. Nothing reopens it on this connection, so say so out loud.
					console.warn("broadcast discovery failed", err);
				}

				// The stream ended, or this run was torn down (its cleanup already ran). Either
				// way there is nothing left announcing the path, so don't hold a dead broadcast.
				offline();
			});
		});
	}

	/** Closes the handle and the broadcast it currently holds. Idempotent. */
	close() {
		this.#signals.close();
	}
}
