import { type Dispose, Effect, type Getter, Signal } from "@moq/signals";
import * as Announce from "../announced.ts";
import { error, SessionCode, SessionError } from "../error.ts";
import type { Consumer as OriginConsumer, Producer as OriginProducer } from "../origin.ts";
import type * as Path from "../path.ts";
import { empty as emptyPath } from "../path.ts";
import { type ConnectProps, connect, type WebSocketOptions, type WebTransportProps } from "./connect.ts";
import type { Established } from "./established.ts";
import type { Probe, Stats } from "./stats.ts";

/**
 * Exponential backoff settings for {@link Reload}'s reconnect loop.
 *
 * The delays carry jitter, so a fleet of tabs knocked offline together doesn't reconnect in
 * lockstep. Every failure is retried; {@link ReloadDelay.timeout} is what stops the loop.
 */
export type ReloadDelay = {
	/** The delay in milliseconds before reconnecting (default: 1000). */
	initial?: DOMHighResTimeStamp;

	/** The multiplier for the delay (default: 2). */
	multiplier?: number;

	/** The maximum delay in milliseconds (default: 5000). */
	max?: DOMHighResTimeStamp;

	/**
	 * Maximum total time in milliseconds to spend retrying before giving up (default:
	 * 10000). Resets after each successful connection. Set to 0 for unlimited retries.
	 */
	timeout?: DOMHighResTimeStamp;
};

/**
 * Connection and retry options for {@link Reload}.
 *
 * {@link ConnectProps.transport} is excluded: a supplied session is good for exactly one
 * connection, so the reconnect loop has nothing to reuse once that session drops. Call
 * {@link connect} directly when you have a session to hand over.
 */
export type ReloadProps = Omit<ConnectProps, "signal" | "transport"> & {
	/** A reload owns the abort signal for each connection attempt. */
	signal?: never;

	/** A one-shot transport cannot be reused by the reconnect loop. */
	transport?: never;

	/** Whether to reload the connection when it disconnects (default: true). */
	enabled?: boolean | Signal<boolean>;

	/** The URL of the relay server. */
	url?: URL | Signal<URL | undefined>;

	/** Backoff settings for the reconnect loop; every field falls back to its default. */
	delay?: ReloadDelay;
};

/**
 * The backoff applied to whichever {@link ReloadDelay} fields a caller leaves out.
 *
 * The timeout is short on purpose: a failure that clears within it was transient, and one that
 * doesn't should surface as an error rather than leave the page silently reconnecting for
 * minutes. A loop nobody watches wants `timeout: 0` instead, since there is no one to react.
 */
const DEFAULT_DELAY: Required<ReloadDelay> = {
	initial: 1000,
	multiplier: 2,
	max: 5000,
	timeout: 10000,
};

/** Current state of a {@link Reload} connection. */
export type ReloadStatus = "connecting" | "connected" | "disconnected";

/** Maintains a MoQ connection, reconnecting with exponential backoff when it drops. */
export class Reload {
	/** Relay URL to connect to; updating it triggers a reconnect. */
	url: Signal<URL | undefined>;

	/** Whether reconnecting is active. */
	enabled: Signal<boolean>;

	/** Current connection status. */
	status = new Signal<ReloadStatus>("disconnected");

	/** The currently established session, or undefined while disconnected. */
	established = new Signal<Established | undefined>(undefined);

	/**
	 * The current connection's PROBE estimates, spanning reconnects.
	 *
	 * Undefined while disconnected: the estimates belong to a single connection.
	 * See {@link Established.probe}.
	 */
	readonly probe: Getter<Probe | undefined>;

	/** WebTransport options applied to each connection attempt (not reactive). */
	webtransport?: WebTransportProps;

	/** WebSocket fallback options applied to each connection attempt (not reactive). */
	websocket: WebSocketOptions | undefined;

	/**
	 * Whether the relay supports broadcast discovery, applied to each connection attempt (not
	 * reactive). Undefined defers to the default for the URL. See {@link Established.discovery}.
	 */
	discovery?: boolean;

	/**
	 * The origin whose broadcasts are served, spanning reconnects (not reactive).
	 *
	 * Each session announces the origin's table when it attaches, so a broadcast published
	 * while offline surfaces on the next connection and a reconnect re-announces everything
	 * still published. See the `publish` connect option.
	 */
	publish?: OriginConsumer;

	/**
	 * The origin fed with the peer's announced broadcasts, spanning reconnects (not
	 * reactive).
	 *
	 * The entries a session fed retract when it dies, and the next session re-populates the
	 * table, so a consumer watching the origin sees offline/online transitions across a
	 * reconnect. See the `subscribe` connect option.
	 */
	subscribe?: OriginProducer;

	/** Backoff settings for the reconnect loop; an unset field uses its default. */
	delay: ReloadDelay;

	/** The reactive effect scope driving the connect loop; closed by {@link Reload.close}. */
	#signals = new Effect();

	/**
	 * Resolves when the reconnect loop stops via {@link Reload.close}.
	 *
	 * Rejects when the loop gives up instead, carrying the failure that was in flight when the
	 * retry window expired.
	 */
	closed: Promise<void>;
	#closedResolve!: () => void;
	#closedReject!: (err: Error) => void;

	// Releases the subscribe origin's expectation. Idempotent, so the terminal paths and the
	// close cleanup can both call it.
	#expected?: Dispose;

	// The current wait between attempts, doubling per failure, and when the retry window expires.
	// Both are undefined between sequences, so a later edit to `delay` applies to the next one.
	#delay: DOMHighResTimeStamp | undefined;
	#deadline: DOMHighResTimeStamp | undefined;

	// Increased by 1 each time to trigger a reload.
	#tick = new Signal(0);

	// True after the browser freezes or hides the page until it visibly resumes.
	#suspended = new Signal(false);

	// Use the serialized URL as the reactive connection key. URL objects use identity
	// equality, but replacing one with an equivalent instance should not reconnect.
	#url: Getter<string | undefined>;
	constructor(props?: ReloadProps) {
		this.url = Signal.from(props?.url);
		this.enabled = Signal.from(props?.enabled ?? true);
		this.delay = props?.delay ?? {};
		this.webtransport = props?.webtransport;
		this.websocket = props?.websocket;
		this.discovery = props?.discovery;
		this.publish = props?.publish;
		this.subscribe = props?.subscribe;

		// Requests on the subscribe origin stay pending across a reconnect, and before the
		// first session establishes, rather than reading as unroutable the moment no session
		// is attached. Released once nothing is coming any more, which is either a close or a
		// terminal failure: a reconnect loop that has given up must stop claiming it will
		// answer, or every request on the origin waits forever on a connection that is done.
		if (this.subscribe) {
			this.#expected = this.subscribe.expect();
			this.#signals.cleanup(this.#expected);
		}

		this.closed = new Promise((resolve, reject) => {
			this.#closedResolve = resolve;
			this.#closedReject = reject;
		});

		// A caller is free to never await `closed`, and giving up rejects it unprompted. Marking the
		// rejection handled here keeps that from surfacing as an `unhandledrejection`; a consumer
		// awaiting the same promise still receives it.
		this.closed.catch(() => {});

		if (typeof window !== "undefined" && typeof document !== "undefined") {
			this.#signals.event(window, "pagehide", () => this.#suspended.set(true));
			this.#signals.event(window, "pageshow", () => this.#suspended.set(false));
			this.#signals.event(window, "unload", () => this.#suspended.set(true));
			this.#signals.event(document, "visibilitychange", () => {
				if (!document.hidden) this.#suspended.set(false);
			});
		}

		this.probe = this.#signals.computed((effect) => {
			const connection = effect.get(this.established);
			return connection && effect.get(connection.probe);
		});

		this.#url = this.#signals.computed((effect) => effect.get(this.url)?.href);
		// Create a reactive root so cleanup is easier.
		this.#signals.run(this.#connect.bind(this));
	}

	#connect(effect: Effect): void {
		// Will retry when the tick changes.
		effect.get(this.#tick);

		const suspended = effect.get(this.#suspended);
		const enabled = effect.get(this.enabled);
		if (!enabled || suspended) return;

		const href = effect.get(this.#url);
		if (!href) return;
		const url = new URL(href);

		effect.set(this.status, "connecting", "disconnected");

		// This run's teardown, handed to connect() so a rerun cancels the attempt in flight.
		const signal = effect.abort;

		effect.spawn(async () => {
			// Set once the session is live, so #retry can tell a healthy session that
			// later dropped from a connect failure or a peer that flaps immediately.
			let connected: DOMHighResTimeStamp | undefined;

			try {
				const connection = await connect(url, {
					websocket: this.websocket,
					webtransport: this.webtransport,
					discovery: this.discovery,
					publish: this.publish,
					subscribe: this.subscribe,
					signal,
				});

				// Hand the connection to the effect, which closes it now if this run is already over.
				effect.cleanup(() => connection.close());
				if (signal.aborted) return;

				effect.set(this.established, connection);
				effect.set(this.status, "connected", "disconnected");

				connected = performance.now();

				// A cancelled effect resolves undefined, so the sentinel tells the session
				// closing (null for clean, an Error otherwise) apart from this run being
				// torn down.
				const closed = await Promise.race([effect.cancel, connection.closed]);
				if (closed === undefined) return;

				console.warn("connection closed, reconnecting");
				this.#retry(effect, connected, closed ?? undefined);
			} catch (err) {
				// Treat teardown as cancellation, not a connection failure.
				if (signal.aborted) return;

				console.warn("connection error:", err);
				this.#retry(effect, connected, err);
			}
		});
	}

	/**
	 * Schedule the next connect attempt after the current backoff, or stop once the retry window
	 * has expired. `connected` is when the dead session was established, if it ever was, and
	 * `cause` the error that killed it, if it died with one.
	 */
	#retry(effect: Effect, connected: DOMHighResTimeStamp | undefined, cause?: unknown): void {
		// Resolved per sequence rather than at construction, so an edit to `delay` (including
		// one that drops a field back to its default) applies to the next retry. Field by
		// field rather than by spread: a caller building `{ initial: maybeInitial }` from an
		// optional value passes an explicit undefined, which a spread would take as the
		// answer, turning the backoff into NaN or the window into forever.
		const delay = this.delay ?? {};
		const initial = delay.initial ?? DEFAULT_DELAY.initial;
		const multiplier = delay.multiplier ?? DEFAULT_DELAY.multiplier;
		const max = delay.max ?? DEFAULT_DELAY.max;
		const timeout = delay.timeout ?? DEFAULT_DELAY.timeout;

		// Any session is dead now: report disconnected during the backoff rather than
		// when the retry reruns the effect.
		this.established.set(undefined);
		this.status.set("disconnected");

		// A session that outlived the initial delay was healthy, so clear the backoff and
		// start a fresh retry window: a one-off drop should reconnect promptly. Anything
		// shorter is a peer that accepts and immediately severs, which has to keep
		// escalating or we hammer it forever at the initial delay.
		if (connected !== undefined && performance.now() - connected >= initial) {
			this.#delay = undefined;
			this.#deadline = undefined;
		}

		// An auth rejection is terminal however long the session lived. UNAUTHORIZED is a
		// specified code rather than one we guessed at, so this is the peer saying these
		// credentials will never work; retrying them just burns the window. Matches
		// moq-tokio's reconnect loop, which stops on the same close.
		//
		// Only a session close says that. The stream registry gives 2 to DELIVERY_TIMEOUT,
		// so a stream reset during the SETUP exchange would otherwise suppress reconnect
		// for good.
		if (cause instanceof SessionError && cause.code === SessionCode.Unauthorized) {
			console.warn("session rejected as unauthorized, not retrying");
			this.#expected?.();
			this.#closedReject(cause);
			return;
		}

		const now = performance.now();
		this.#delay ??= initial;
		this.#deadline ??= timeout > 0 ? now + timeout : Number.POSITIVE_INFINITY;

		if (now >= this.#deadline) {
			console.warn("reconnect timed out");
			// A graceful close has no error, so report the timeout itself.
			this.#expected?.();
			this.#closedReject(cause === undefined ? new Error("reconnect timed out") : error(cause));
			return;
		}

		// Equal jitter, so a fleet of tabs knocked offline together doesn't reconnect on the same
		// tick, and never past the deadline the retry window promised.
		const wait = Math.min(this.#delay * (0.5 + Math.random() / 2), this.#deadline - now);
		this.#delay = Math.min(this.#delay * multiplier, max);

		const tick = this.#tick.peek() + 1;
		effect.timer(() => this.#tick.update((prev) => Math.max(prev, tick)), wait);
	}

	/**
	 * Subscribe to broadcast announcements under an optional prefix, spanning reconnects.
	 *
	 * The same {@link Announce.Consumer} stream as {@link Established.announced}, but everything active
	 * is retracted (an `active: false` update) whenever the connection drops and re-announced on
	 * reconnect, so a consumer draining `next()` never clings to a dead route across a reconnect.
	 *
	 * Stays empty while the relay lacks {@link Established.discovery}.
	 */
	announced(prefix: Path.Valid = emptyPath()): Announce.Consumer {
		// With a subscribe origin the table already spans reconnects (the forwarder retracts
		// a dead session's entries), so its stream is the same thing with less machinery.
		if (this.subscribe) return this.subscribe.announced(prefix);

		const producer = new Announce.Producer(prefix);
		const consumer = producer.consume();

		// Closing the consumer closes the shared state, so stop appending after that.
		let closed = false;
		void consumer.closed.then(() => {
			closed = true;
		});

		const pump = new Effect();
		pump.run((effect) => {
			const conn = effect.get(this.established);
			if (!conn) return;

			// Without discovery the upstream announce stream never yields, so leave the
			// consumer empty rather than opening a subscription that can't be answered.
			if (!conn.discovery) return;

			const upstream = conn.announced(prefix);
			effect.cleanup(() => upstream.close());

			// Track what this connection announced so we can retract it if the connection drops.
			const active = new Set<Path.Valid>();

			effect.spawn(async () => {
				try {
					for (;;) {
						const entry = await Promise.race([effect.cancel, upstream.next()]);
						if (!entry) break;
						if (entry.active) active.add(entry.path);
						else active.delete(entry.path);
						producer.append(entry);
					}
				} catch {
					// A dropped connection resets the announce stream; the retractions below cover it.
				} finally {
					// Retract everything from the connection that just went away, so a per-broadcast
					// watcher tears down instead of clinging to the dead route.
					if (!closed) {
						for (const path of active) {
							producer.append({ path, active: false });
						}
					}
				}
			});
		});

		this.#signals.cleanup(() => pump.close());
		void consumer.closed.then(() => pump.close());

		return consumer;
	}

	/**
	 * A reactive handle to one broadcast, spanning reconnects.
	 *
	 * The same {@link Announce.Broadcast} as {@link Established.announcedBroadcast}, but it
	 * follows the reconnect loop: the broadcast drops to `undefined` when the connection dies
	 * and resolves again once the new connection announces the path. Use it instead of
	 * consuming off {@link Reload.established} whenever the broadcast may come online after you
	 * do, which is exactly the case a blind `consume` loses.
	 *
	 * Close the handle when done; {@link Reload.close} only drops it to `undefined`.
	 */
	announcedBroadcast(path: Path.Valid): Announce.Broadcast {
		// Same delegation as announced(): the origin's table is the reconnect-spanning view.
		if (this.subscribe) return new Announce.Broadcast({ origin: this.subscribe, path });
		return new Announce.Broadcast({ connection: this.established, path });
	}

	/**
	 * Snapshot the live connection's transport counters, or undefined while disconnected.
	 * See {@link Established.stats}.
	 */
	async stats(): Promise<Stats | undefined> {
		return this.established.peek()?.stats();
	}

	/** Stop reconnecting, close the current connection, and resolve {@link Reload.closed}. */
	close() {
		this.#signals.close();
		this.#closedResolve();
	}
}
