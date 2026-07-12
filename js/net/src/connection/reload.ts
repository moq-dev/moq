import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Path from "../path.ts";
import { empty as emptyPath } from "../path.ts";
import { type ConnectProps, connect, type WebSocketOptions, type WebTransportProps } from "./connect.ts";
import type { Established } from "./established.ts";

/** Exponential backoff settings for {@link Reload}'s reconnect loop. */
export type ReloadDelay = {
	// The delay in milliseconds before reconnecting.
	// default: 1000
	initial: DOMHighResTimeStamp;

	// The multiplier for the delay.
	// default: 2
	multiplier: number;

	// The maximum delay in milliseconds.
	// default: 30000
	max: DOMHighResTimeStamp;

	// Maximum total time in milliseconds to spend retrying before giving up.
	// Resets after each successful connection. Set to 0 for unlimited retries.
	// default: 300000 (5 minutes)
	timeout?: DOMHighResTimeStamp;
};

/** Options for {@link Reload}: connect options plus reactive URL/enabled signals and backoff tuning. */
export type ReloadProps = ConnectProps & {
	// Whether to reload the connection when it disconnects.
	// default: true
	enabled?: boolean | Signal<boolean>;

	// The URL of the relay server.
	url?: URL | Signal<URL | undefined>;

	// The delay for the reload.
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

	// All actively announced broadcast paths, updated reactively.
	#announced = new Signal<Set<Path.Valid>>(new Set());

	/** The set of broadcast paths currently announced by the server, updated reactively. */
	readonly announced: Getter<Set<Path.Valid>> = this.#announced;

	/** WebTransport options applied to each connection attempt (not reactive). */
	webtransport?: WebTransportProps;

	/** WebSocket fallback options applied to each connection attempt (not reactive). */
	websocket: WebSocketOptions | undefined;

	/** Backoff settings for the reconnect loop. */
	delay: ReloadDelay;

	/** The reactive effect scope driving the connect loop; closed by {@link Reload.close}. */
	signals = new Effect();

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

	constructor(props?: ReloadProps) {
		this.url = Signal.from(props?.url);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.delay = props?.delay ?? { initial: 1000, multiplier: 2, max: 30000 };
		this.webtransport = props?.webtransport;
		this.websocket = props?.websocket;

		this.#delay = this.delay.initial;

		this.closed = new Promise((resolve, reject) => {
			this.#closedResolve = resolve;
			this.#closedReject = reject;
		});

		if (typeof window !== "undefined" && typeof document !== "undefined") {
			this.signals.event(window, "pagehide", () => this.#suspended.set(true));
			this.signals.event(window, "pageshow", () => this.#suspended.set(false));
			this.signals.event(window, "unload", () => this.#suspended.set(true));
			this.signals.event(document, "visibilitychange", () => {
				if (!document.hidden) this.#suspended.set(false);
			});
		}

		// Create a reactive root so cleanup is easier.
		this.signals.run(this.#connect.bind(this));
		this.signals.run(this.#runAnnounced.bind(this));
	}

	#connect(effect: Effect): void {
		// Will retry when the tick changes.
		effect.get(this.#tick);

		const suspended = effect.get(this.#suspended);
		const enabled = effect.get(this.enabled);
		if (!enabled || suspended) return;

		const url = effect.get(this.url);
		if (!url) return;

		effect.set(this.status, "connecting", "disconnected");

		effect.spawn(async () => {
			try {
				const pending = connect(url, { websocket: this.websocket, webtransport: this.webtransport });

				const connection = await Promise.race([effect.cancel, pending]);
				if (!connection) {
					pending.then((conn) => conn.close()).catch(() => {});
					return;
				}

				effect.set(this.established, connection);
				effect.cleanup(() => connection.close());

				effect.set(this.status, "connected", "disconnected");

				// Reset the exponential backoff and timeout on success.
				this.#delay = this.delay.initial;
				this.#retryStart = undefined;

				await Promise.race([effect.cancel, connection.closed]);
			} catch (err) {
				console.warn("connection error:", err);

				// Track retry start for timeout.
				this.#retryStart ??= performance.now();

				const timeout = this.delay.timeout ?? 300000;
				if (timeout > 0) {
					const elapsed = performance.now() - this.#retryStart;
					if (elapsed >= timeout) {
						console.warn("reconnect timed out");
						this.#closedReject(err instanceof Error ? err : new Error(String(err)));
						return;
					}
				}

				const tick = this.#tick.peek() + 1;
				effect.timer(() => this.#tick.update((prev) => Math.max(prev, tick)), this.#delay);

				this.#delay = Math.min(this.#delay * this.delay.multiplier, this.delay.max);
			}
		});
	}

	#runAnnounced(effect: Effect): void {
		this.#announced.set(new Set());

		const conn = effect.get(this.established);
		if (!conn) return;

		effect.cleanup(() => this.#announced.set(new Set()));

		// Cloudflare's relay does not yet support SUBSCRIBE_NAMESPACE, so
		// skip announce subscriptions entirely for those hosts.
		if (conn.url.hostname.endsWith("mediaoverquic.com")) {
			return;
		}

		const announced = conn.announced(emptyPath());
		effect.cleanup(() => announced.close());

		effect.spawn(async () => {
			try {
				for (;;) {
					const entry = await Promise.race([effect.cancel, announced.next()]);
					if (!entry) break;

					this.#announced.mutate((active) => {
						if (entry.active) {
							active.add(entry.path);
						} else {
							active.delete(entry.path);
						}
					});
				}
			} catch (err) {
				this.#announced.set(new Set());
				throw err;
			}
		});
	}

	/** Stop reconnecting, close the current connection, and resolve {@link Reload.closed}. */
	close() {
		this.signals.close();
		this.#closedResolve();
	}
}
