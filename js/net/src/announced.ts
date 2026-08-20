/**
 * Broadcast announcement streams: which broadcast paths are available under a prefix.
 *
 * @module
 */
import { Effect, type GetPromise, type Getter, type GetterInit, getter, Once, Signal } from "@moq/signals";
import type * as broadcast from "./broadcast.js";
import type { Established } from "./connection/established.js";
import type { Request as OriginRequest, Table as OriginTable } from "./origin.js";
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

/**
 * What to watch, for {@link Broadcast}: a path on exactly one source, enforced by the
 * union so a call with neither or both does not compile.
 *
 * @public
 */
export type BroadcastProps = {
	/** The broadcast path to watch. */
	path: Path.Valid;
} & (
	| {
			/**
			 * The connection to watch on. Accepts a live {@link Established} session, or a
			 * reactive one (a `Connection.Reload`'s `established`), which is how the handle
			 * survives reconnects.
			 */
			connection: GetterInit<Established | undefined>;
			origin?: undefined;
	  }
	| {
			/**
			 * The origin to watch instead of a session.
			 *
			 * The handle then follows the origin's table: it resolves whenever anything
			 * routes the path (a local publish, or any session feeding the origin), which is
			 * how it spans reconnects without watching the connection itself. While every
			 * attached session lacks discovery it falls back to a standing request, so
			 * `active` means assumed present.
			 */
			origin: GetterInit<OriginTable | undefined>;
			connection?: undefined;
	  }
);

/**
 * A reactive handle to a single broadcast: {@link Broadcast.active} holds a live
 * {@link broadcast.Consumer} while the path is announced and `undefined` while nobody
 * publishes it.
 *
 * Use this instead of {@link Established.consume} whenever the broadcast may not exist yet.
 * Subscribing to a path nobody publishes gets the stream reset, so a consumer that races the
 * publisher stays silent forever unless it retries; this waits for the announcement instead.
 *
 * A same-name republish re-consumes, so the handle attaches to the new instance rather than
 * clinging to the dead one. A relay failover that keeps the same publisher does *not*: the
 * subscription resumes across the new route, so `active` holds the same consumer throughout and
 * never goes offline. Only a change of publisher produces an offline/online transition.
 *
 * Built from a reconnecting `Connection.Reload`, the handle also spans reconnects: the broadcast
 * drops to `undefined` while disconnected and resolves again once the new connection announces it.
 *
 * Falls back to consuming blind (and warns once) on a relay without
 * {@link Established.discovery}, where there is no announcement to wait for. `active` then
 * means *assumed present* rather than known live: nothing reports whether the path exists, so
 * a subscribe to a missing broadcast is how a caller finds out. The handle stays usable either
 * way, and because it is scoped to the path rather than to one publisher, a subscribe made
 * after a publisher finally appears succeeds.
 *
 * If discovery fails on a live session (the announcement stream is reset, or the relay
 * refuses it) a connection-backed handle goes offline and stays there: nothing reopens the
 * stream on that connection. Build it from a `Connection.Reload` if you need it to recover,
 * since a new connection starts a new stream. An origin-backed handle recovers on its own:
 * the session stops counting as discovering, so the handle falls back to a standing request.
 *
 * Close it to release the announcement stream and the current broadcast.
 *
 * @public
 */
export class Broadcast {
	/** The broadcast path this handle watches. */
	readonly path: Path.Valid;

	/**
	 * The live broadcast, or `undefined` while it is offline.
	 *
	 * Borrowed, not yours to close: this handle owns the consumer and swaps it when the path is
	 * republished. `active` keeps pointing at whatever you closed, so once you drop the last
	 * reference the shared broadcast is gone and reads fail until the next announcement replaces
	 * it. Take a {@link broadcast.Consumer.clone} for a lifetime of your own, or close this whole
	 * handle to release everything.
	 */
	readonly active: Getter<broadcast.Consumer | undefined>;

	#active = new Signal<broadcast.Consumer | undefined>(undefined);
	#signals = new Effect();

	/**
	 * Watch a path on a connection or an origin.
	 *
	 * Prefer `announcedBroadcast(path)` on the connection itself. Reach for this when the
	 * source you want to follow isn't either connection type, e.g. your own
	 * `Getter<Established | undefined>` or an origin fed by a `subscribe` option.
	 */
	constructor({ connection, path, origin }: BroadcastProps) {
		this.path = path;
		this.active = this.#active;

		if (origin) {
			const source = getter(origin);
			this.#signals.run((effect) => this.#runOrigin(effect, source));
			return;
		}

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

				// The announcement-gated path below goes offline when the stream ends with the
				// session; without discovery there is no stream, so watch the session itself.
				// A consumed broadcast is a path-scoped handle, not a subscription, so its own
				// `closed` says nothing about whether the path exists or the session is alive.
				// Raced against the run's teardown so a closed handle isn't retained until the
				// session ends; the cleanup above has already cleared `active` in that case.
				effect.spawn(async () => {
					await Promise.race([effect.cancel, conn.closed]);
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

	// Follow the origin's table instead of a session's announce stream. The table already
	// merges every source (local publishes, every feeding session), so this is simpler than
	// the session path: no hop bookkeeping, and the table's identity-diffed announcements
	// retract before a republish, which is what lets a plain re-consume suffice.
	#runOrigin(effect: Effect, source: Getter<OriginTable | undefined>): void {
		const origin = effect.get(source);
		if (!origin) return;

		// The two ways the broadcast can resolve. The table wins: it is knowledge (a local
		// publish or an announcement) while a request's answer is only assumed present.
		const table = new Signal<broadcast.Consumer | undefined>(undefined);
		const requested = new Signal<broadcast.Consumer | undefined>(undefined);
		effect.run((nested) => {
			nested.set(this.#active, nested.get(table) ?? nested.get(requested), undefined);
		});

		// Follow the table regardless of sessions: a local publish resolves with no
		// connection at all (and keeps resolving while one reconnects), and the
		// identity-diffed announcements swap the handle on a republish.
		const announced = origin.announced(this.path);
		effect.cleanup(() => announced.close());

		let current: broadcast.Consumer | undefined;

		// Held open while the path is announced. A request resolves to the table's route when
		// there is one, and a session skips answering a path the table routes, so within the
		// announced window this can only ever produce the announced broadcast.
		let request: OriginRequest | undefined;

		const offline = () => {
			current?.close();
			current = undefined;
			request?.close();
			request = undefined;
			table.set(undefined);
		};
		effect.cleanup(offline);

		effect.spawn(async () => {
			for (;;) {
				const event = await Promise.race([effect.cancel, announced.next()]);
				if (!event) break;

				// Scoped to `path`, so the exact broadcast arrives with an empty suffix; ignore children.
				if (event.path !== Path.empty()) continue;

				if (event.active) {
					current?.close();
					request ??= origin.request(this.path);
					// Cloned: the request borrows the table's front, and this handle owns what
					// it hands out.
					current = request.active.peek()?.clone();
					table.set(current);
				} else {
					offline();
				}
			}

			// The origin closed, or this run was torn down. Either way nothing routes the path.
			offline();
		});

		// Blind fallback: while any attached session cannot announce, the table is an
		// incomplete picture of what is reachable, so stand a request for whichever session
		// answers. Gated on exactly `false`: with no session there is nobody to ask, and with
		// every session announcing the gate is the point, so a blind subscribe would defeat it.
		effect.run((nested) => {
			if (nested.get(origin.discovery) !== false) return;

			const request = origin.request(this.path);
			nested.cleanup(() => request.close());
			nested.run((inner) => {
				inner.set(requested, inner.get(request.active), undefined);
			});
		});
	}

	/** Resolves once the handle is closed, so an owner can drop its reference. */
	get closed(): Promise<void> {
		return this.#signals.closed;
	}

	/** Closes the handle and the broadcast it currently holds. Idempotent. */
	close() {
		this.#signals.close();
	}
}
