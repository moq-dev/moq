import { Effect, type Getter, Signal } from "@moq/signals";
import * as Announce from "../announced.ts";
import type * as Path from "../path.ts";
import { empty as emptyPath } from "../path.ts";
import { type ConnectProps, connect, type WebSocketOptions, type WebTransportProps } from "./connect.ts";
import type { Established } from "./established.ts";

/** Exponential backoff settings for {@link Reload}'s reconnect loop. */
export type ReloadDelay = {
	/** The delay in milliseconds before reconnecting (default: 1000). */
	initial: DOMHighResTimeStamp;

	/** The multiplier for the delay (default: 2). */
	multiplier: number;

	/** The maximum delay in milliseconds (default: 30000). */
	max: DOMHighResTimeStamp;

	/**
	 * Maximum total time in milliseconds to spend retrying before giving up (default:
	 * 300000, 5 minutes). Resets after each successful connection. Set to 0 for
	 * unlimited retries.
	 */
	timeout?: DOMHighResTimeStamp;
};

/** Options for {@link Reload}: connect options plus reactive URL/enabled signals and backoff tuning. */
export type ReloadProps = ConnectProps & {
	/** Whether to reload the connection when it disconnects (default: true). */
	enabled?: boolean | Signal<boolean>;

	/** The URL of the relay server. */
	url?: URL | Signal<URL | undefined>;

	/** Backoff settings for the reconnect loop. */
	delay?: ReloadDelay;
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

	/** WebTransport options applied to each connection attempt (not reactive). */
	webtransport?: WebTransportProps;

	/** WebSocket fallback options applied to each connection attempt (not reactive). */
	websocket: WebSocketOptions | undefined;

	/**
	 * Whether the relay supports broadcast discovery, applied to each connection attempt (not
	 * reactive). Undefined defers to the default for the URL. See {@link Established.discovery}.
	 */
	discovery?: boolean;

	/** Backoff settings for the reconnect loop. */
	delay: ReloadDelay;

	/** The reactive effect scope driving the connect loop; closed by {@link Reload.close}. */
	#signals = new Effect();

	/** Resolves when the reconnect loop stops via {@link Reload.close} or the retry timeout. */
	closed: Promise<void>;
	#closedResolve!: () => void;
	#closedReject!: (err: Error) => void;

	#delay: DOMHighResTimeStamp;

	// Timestamp when the current retry sequence started (for timeout).
	#retryStart: DOMHighResTimeStamp | undefined;

	// Increased by 1 each time to trigger a reload.
	#tick = new Signal(0);

	// True after the browser freezes or hides the page until it visibly resumes.
	#suspended = new Signal(false);

	// Use the serialized URL as the reactive connection key. URL objects use identity
	// equality, but replacing one with an equivalent instance should not reconnect.
	#url: Getter<string | undefined>;
	constructor(props?: ReloadProps) {
		this.url = Signal.from(props?.url);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.delay = props?.delay ?? { initial: 1000, multiplier: 2, max: 30000 };
		this.webtransport = props?.webtransport;
		this.websocket = props?.websocket;
		this.discovery = props?.discovery;

		this.#delay = this.delay.initial;

		this.closed = new Promise((resolve, reject) => {
			this.#closedResolve = resolve;
			this.#closedReject = reject;
		});

		if (typeof window !== "undefined" && typeof document !== "undefined") {
			this.#signals.event(window, "pagehide", () => this.#suspended.set(true));
			this.#signals.event(window, "pageshow", () => this.#suspended.set(false));
			this.#signals.event(window, "unload", () => this.#suspended.set(true));
			this.#signals.event(document, "visibilitychange", () => {
				if (!document.hidden) this.#suspended.set(false);
			});
		}

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

		effect.spawn(async () => {
			// Set once the session is live, so #retry can tell a healthy session that
			// later dropped from a connect failure or a peer that flaps immediately.
			let connected: DOMHighResTimeStamp | undefined;

			try {
				const pending = connect(url, {
					websocket: this.websocket,
					webtransport: this.webtransport,
					discovery: this.discovery,
				});

				const connection = await Promise.race([effect.cancel, pending]);
				if (!connection) {
					pending.then((conn) => conn.close()).catch(() => {});
					return;
				}

				effect.set(this.established, connection);
				effect.cleanup(() => connection.close());

				effect.set(this.status, "connected", "disconnected");

				connected = performance.now();

				// A cancelled effect resolves undefined, so the sentinel tells the session
				// closing apart from this run being torn down.
				const closed = await Promise.race([effect.cancel, connection.closed.then(() => true)]);
				if (!closed) return;

				console.warn("connection closed, reconnecting");
				this.#retry(effect, connected);
			} catch (err) {
				console.warn("connection error:", err);
				this.#retry(effect, connected, err);
			}
		});
	}

	/**
	 * Schedule the next connect attempt after the current backoff, or give up when the
	 * retry window has expired. `connected` is when the dead session was established, if
	 * it ever was, and `cause` the error that killed it, if it died with one.
	 */
	#retry(effect: Effect, connected: DOMHighResTimeStamp | undefined, cause?: unknown): void {
		// Any session is dead now: report disconnected during the backoff rather than
		// when the retry reruns the effect.
		this.established.set(undefined);
		this.status.set("disconnected");

		// A session that outlived the initial delay was healthy, so clear the backoff and
		// start a fresh retry window: a one-off drop should reconnect promptly. Anything
		// shorter is a peer that accepts and immediately severs, which has to keep
		// escalating or we hammer it forever at the initial delay.
		if (connected !== undefined && performance.now() - connected >= this.delay.initial) {
			this.#delay = this.delay.initial;
			this.#retryStart = undefined;
		}

		// Track retry start for timeout.
		this.#retryStart ??= performance.now();

		const timeout = this.delay.timeout ?? 300000;
		if (timeout > 0) {
			const elapsed = performance.now() - this.#retryStart;
			if (elapsed >= timeout) {
				console.warn("reconnect timed out");
				// A graceful close has no error, so report the timeout itself.
				if (cause === undefined) this.#closedReject(new Error("reconnect timed out"));
				else this.#closedReject(cause instanceof Error ? cause : new Error(String(cause)));
				return;
			}
		}

		const tick = this.#tick.peek() + 1;
		effect.timer(() => this.#tick.update((prev) => Math.max(prev, tick)), this.#delay);

		this.#delay = Math.min(this.#delay * this.delay.multiplier, this.delay.max);
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

	/** Stop reconnecting, close the current connection, and resolve {@link Reload.closed}. */
	close() {
		this.#signals.close();
		this.#closedResolve();
	}
}
