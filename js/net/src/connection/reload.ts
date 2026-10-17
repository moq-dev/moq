import { Instant, Milli, Nano, now, sleep } from "@moq/clock";
import { Effect, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import * as Announce from "../announced.ts";
import { Allocator } from "../bandwidth.ts";
import { error, SessionCode, SessionError } from "../error.ts";
import type { Consumer as OriginConsumer, Producer as OriginProducer } from "../origin.ts";
import * as Path from "../path.ts";
import { Follow } from "../util/observed.ts";
import { wireOf } from "../wire.ts";
import { type ConnectProps, connect, type WebSocketProps, type WebTransportProps } from "./connect.ts";
import type { Established } from "./established.ts";
import { DISCOVERY_REMOVED } from "./forward.ts";
import { DEFAULT_HANDOVER, type Drain, dialed, type GoawayProps, handover, pinnedTransport, target } from "./goaway.ts";
import type { Probe, Stats } from "./stats.ts";

/**
 * Exponential backoff settings for the reconnect loop.
 *
 * The delays carry jitter, so a fleet of tabs knocked offline together doesn't reconnect in
 * lockstep. Every failure is retried; {@link ReloadDelay.timeout} is what stops the current
 * URL. A new URL or a disable/re-enable starts another sequence.
 */
export type ReloadDelay = {
	/** The delay before reconnecting (default: 1000ms). */
	initial?: Milli;

	/** The multiplier for the delay (default: 2). */
	multiplier?: number;

	/** The maximum delay (default: 5000ms). */
	max?: Milli;

	/**
	 * Maximum total time to spend retrying the current URL before giving up
	 * (default: 0, unlimited). Resets after each successful connection, a URL change, or a
	 * disable/re-enable.
	 */
	timeout?: Milli;
};

/**
 * Connection and retry options for {@link Reload}.
 *
 * {@link ConnectProps.transport} is excluded: a supplied session is good for exactly one
 * connection, so the reconnect loop has nothing to reuse once that session drops. Call
 * {@link connect} directly when you have a session to hand over.
 *
 * @internal
 */
export type ReloadProps = Omit<ConnectProps, "url" | "signal" | "transport"> & {
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

	/** How to react to the peer's GOAWAY; every field falls back to its default. */
	goaway?: GoawayProps;
};

/**
 * The backoff applied to whichever {@link ReloadDelay} fields a caller leaves out.
 *
 * Retries are unlimited by default: a loop that gives up stays offline until something
 * redials it, and a page usually has nothing that will, so an outage of any length has to
 * recover on its own. {@link Reload.status} already shows the outage. A caller that can react
 * sets a timeout to surface a persistent failure on {@link Reload.error}; giving up does not
 * dispose the loop, and a new URL or a disable/re-enable starts another sequence.
 */
const DEFAULT_DELAY: Required<ReloadDelay> = {
	initial: Milli(1000),
	multiplier: 2,
	max: Milli(5000),
	timeout: Milli(0),
};

/** How often the send-rate estimate is sampled from the live transport. */
const BANDWIDTH_POLL = Nano.fromMilli(Milli(100));

/** Current state of a reconnecting connection. */
export type ReloadStatus = "connecting" | "connected" | "disconnected";

/**
 * The reconnect loop behind a Connection handle: connects, waits for the session to end, then
 * redials with exponential backoff.
 *
 * @internal
 */
export class Reload {
	/** Relay URL to connect to; updating it triggers a reconnect. */
	url: Signal<URL | undefined>;

	/** Whether reconnecting is active. */
	enabled: Signal<boolean>;

	/** Current connection status. */
	status = new Signal<ReloadStatus>("disconnected");

	/**
	 * The failure that stopped retrying the current URL, or undefined while the loop is live.
	 *
	 * Set on an auth rejection or when the retry window expires. Cleared when a new URL or a
	 * disable/re-enable starts another sequence, not on a page hide/show. Transient drops
	 * that are still being retried leave this empty.
	 */
	readonly error: Getter<Error | undefined>;

	/** The currently established session, or undefined while disconnected. */
	established = new Signal<Established | undefined>(undefined);

	/**
	 * The current connection's PROBE estimates, spanning reconnects.
	 *
	 * Undefined while disconnected: the estimates belong to a single connection.
	 * Observation requests updates; `peek()` alone is passive.
	 * See {@link Established.probe}.
	 */
	readonly probe: Getter<Probe | undefined>;
	#probeSource = new Signal<Getter<Probe> | undefined>(undefined);

	/**
	 * Divides this connection's send-rate estimate among the tracks sharing it.
	 *
	 * The same instance across reconnects, so reservations survive a drop. The
	 * estimate is `undefined` while disconnected, which tells a sender to hold
	 * its rate rather than encode at zero.
	 */
	readonly bandwidth: Allocator;

	/** WebTransport options applied to each connection attempt (not reactive). */
	webtransport?: WebTransportProps;

	/** WebSocket fallback options applied to each connection attempt (not reactive). */
	websocket: WebSocketProps | undefined;

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
	 * The entries a session fed retract when it dies, and the next session asks again for
	 * whatever is still watched, so a consumer watching the origin sees offline/online
	 * transitions across a reconnect. See the `consume` connect option.
	 */
	consume?: OriginProducer;

	/** Backoff settings for the reconnect loop; an unset field uses its default. */
	delay: ReloadDelay;

	/** How to react to the peer's GOAWAY (not reactive). */
	goaway: GoawayProps;

	/**
	 * The URL an accepted GOAWAY redirect assigned, or undefined while the loop dials
	 * {@link Reload.url}. Sticky across reconnects: a redirect is an assignment, not a
	 * detour. Cleared when a new URL or a disable/re-enable starts another sequence.
	 */
	readonly redirect: Getter<URL | undefined>;

	/** The reactive effect scope driving the connect loop; closed by {@link Reload.close}. */
	#signals = new Effect();

	// Sampled from the live session; undefined while disconnected or the transport has none.
	#estimate = new Signal<number | undefined>(undefined);

	/**
	 * Settles once this loop is disposed via {@link Reload.close}: `null` on a clean close,
	 * or the abort {@link Error}. Peek it synchronously (`undefined` while open), observe it
	 * reactively, or `await` it. Attempt failures live on {@link Reload.error} instead.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#closed;
	}

	#closed = new Once<Error | null>();
	#error = new Signal<Error | undefined>(undefined);
	#redirect = new Signal<URL | undefined>(undefined);
	// The configured href the redirect was assigned for.
	#redirectHref: string | undefined;

	// A session the peer sent GOAWAY on, serving its groups in flight while the replacement
	// dials. Retired when it closes, at its handover cap, or when the sequence ends.
	#draining: Draining | undefined;

	// Whether a connect attempt is in flight, so a retiring predecessor reports the right status.
	#dialing = false;

	// The current wait between attempts, doubling per failure, and when the retry window expires.
	// Both are undefined between sequences, so a later edit to `delay` applies to the next one.
	#delay: Milli | undefined;
	#deadline: number | undefined;

	// The URL the current retry sequence is for. Cleared when disabled, URL-less, or given
	// up, so the next attempt starts a fresh backoff window.
	#sequenceHref: string | undefined;

	// The href that exhausted its retry sequence. Survives a page hide/show so resume does
	// not redial credentials the peer already refused. Cleared when the URL changes or the
	// loop is disabled.
	#givenUpHref: string | undefined;

	// Increased by 1 each time to trigger a reload.
	#tick = new Signal(0);

	// True after the browser freezes or hides the page until it visibly resumes.
	#suspended = new Signal(false);

	// Use the serialized URL as the reactive connection key. URL objects use identity
	// equality, but replacing one with an equivalent instance should not reconnect.
	#url: Getter<string | undefined>;
	constructor(props?: ReloadProps) {
		if (props && "discovery" in props) throw new TypeError(DISCOVERY_REMOVED);
		this.url = Signal.from(props?.url);
		this.enabled = Signal.from(props?.enabled ?? true);
		this.delay = props?.delay ?? {};
		this.goaway = props?.goaway ?? {};
		this.redirect = this.#redirect;
		this.webtransport = props?.webtransport;
		this.websocket = props?.websocket;
		this.publish = props?.publish;
		this.consume = props?.consume;

		// Requests on the consume origin stay pending across a reconnect, and before the
		// first session establishes, rather than reading as unroutable the moment no session
		// is attached. Released only when this loop is disposed: giving up the current URL
		// is recoverable (a new URL or a disable/re-enable starts another sequence), so a
		// request must keep waiting rather than go unroutable in the gap.
		if (this.consume) {
			this.#signals.cleanup(wireOf(this.consume).expect());
		}

		this.error = this.#error;

		const win = globalThis.window;
		const doc = globalThis.document;
		if (typeof win !== "undefined" && typeof doc !== "undefined") {
			this.#signals.event(win, "pagehide", () => this.#suspended.set(true));
			this.#signals.event(win, "pageshow", () => this.#suspended.set(false));
			this.#signals.event(win, "unload", () => this.#suspended.set(true));
			this.#signals.event(doc, "visibilitychange", () => {
				if (!doc.hidden) this.#suspended.set(false);
			});
		}

		this.probe = new Follow(this.#probeSource);
		this.#signals.run((effect) => {
			effect.set(this.#probeSource, effect.get(this.established)?.probe, undefined);
		});

		this.bandwidth = new Allocator(this.#estimate);
		this.#signals.cleanup(() => this.bandwidth.close());

		// The transport has no event for the send-rate estimate, so sample on our
		// own schedule and skip a tick while the previous snapshot is outstanding.
		this.#signals.run((effect) => {
			effect.set(this.#estimate, undefined);
			const connection = effect.get(this.established);
			if (!connection) return;

			let pending = false;
			const sample = async () => {
				if (pending) return;
				pending = true;
				try {
					const stats = await effect.race(connection.stats());
					if (stats) this.#estimate.set(stats.estimatedSendRate);
				} finally {
					pending = false;
				}
			};

			void sample();
			effect.interval(() => void sample(), BANDWIDTH_POLL);
		});

		this.#url = this.#signals.computed((effect) => effect.get(this.url)?.href);
		// Create a reactive root so cleanup is easier.
		this.#signals.run(this.#connect.bind(this));
	}

	#connect(effect: Effect): void {
		// Will retry when the tick changes.
		effect.get(this.#tick);

		const enabled = effect.get(this.enabled);
		if (!enabled) {
			this.#givenUpHref = undefined;
			this.#resetSequence();
			this.#redirect.set(undefined);
			return;
		}

		// Hide/freeze pauses an in-flight sequence without recovering from give-up.
		const suspended = effect.get(this.#suspended);
		if (suspended) {
			this.#resetSequence();
			return;
		}

		const href = effect.get(this.#url);
		if (!href) {
			this.#givenUpHref = undefined;
			this.#resetSequence();
			this.#redirect.set(undefined);
			return;
		}
		const url = new URL(href);

		if (this.#givenUpHref === href) {
			return;
		}

		if (this.#sequenceHref !== href) {
			this.#resetSequence();
			// A redirect was assigned for another URL; this one starts from itself. A page
			// hide/show resumes the same URL, so it keeps the assignment.
			if (this.#redirectHref !== href) this.#redirect.set(undefined);
			this.#sequenceHref = href;
			this.#error.set(undefined);
			this.#givenUpHref = undefined;
		}

		// A drained predecessor still serves while its replacement dials.
		if (!this.#draining) this.status.set("connecting");

		// This run's teardown, handed to connect() so a rerun cancels the attempt in flight.
		const signal = effect.abort;

		// The session this run serves, closed with the run. A drained one leaves this slot
		// for #draining, which outlives the run.
		let current: Established | undefined;
		effect.cleanup(() => {
			if (current) {
				current.abort();
				if (this.established.peek() === current) this.established.set(undefined);
				current = undefined;
			}
			if (this.established.peek() === undefined) this.status.set("disconnected");
		});

		effect.spawn(async () => {
			// Set once the session is live, so #retry can tell a healthy session that
			// later dropped from a connect failure or a peer that flaps immediately.
			let connected: Instant | undefined;

			try {
				// Loops only to migrate: a GOAWAY off a healthy session dials its replacement
				// straight away, with no backoff.
				for (;;) {
					const dialing = this.#redirect.peek() ?? url;

					this.#dialing = true;
					let connection: Established;
					try {
						connection = await connect({
							url: dialing,
							// A redirect names the relay; a fallback URL pinned for the old one does not follow.
							websocket: this.#redirect.peek() ? { ...this.websocket, url: undefined } : this.websocket,
							webtransport: this.webtransport,
							publish: this.publish,
							consume: this.consume,
							signal,
						});
					} finally {
						this.#dialing = false;
					}

					// Hand the connection to the effect, which closes it now if this run is already over.
					if (signal.aborted) {
						connection.abort();
						return;
					}
					current = connection;

					// The replacement serves now; a predecessor keeps draining its groups in flight.
					this.established.set(connection);
					this.status.set("connected");
					connected = now();

					// A cancelled effect resolves undefined, so the sentinel tells the session
					// closing (null for clean, an Error otherwise) apart from this run being
					// torn down. Anything else is the peer's GOAWAY.
					const ended = await effect.race(connection.closed, wireOf(connection).goaway);
					if (ended === undefined) return;
					if (ended === null || ended instanceof Error) {
						console.warn("connection closed, reconnecting");
						if (this.established.peek() === connection) this.established.set(undefined);
						this.#retry(effect, connected, ended ?? undefined);
						return;
					}

					current = undefined;
					// A pinned WebSocket URL can win the race against the primary. Judge the
					// redirect against that endpoint, not the primary we never reached.
					const socket = this.#redirect.peek() ? undefined : this.websocket?.url;
					if (!this.#migrate(connection, dialed(dialing, connection.transport, socket), ended)) return;

					// A session that outlived the initial delay was healthy, so its handover is not a
					// failure. One redirected almost at once still migrates, but through the backoff,
					// so two peers bouncing us between them escalate to the capped delay.
					if (Instant.durationSince(now(), connected) < Nano.fromMilli(this.#initial())) {
						this.#retry(effect, connected, new Error("peer redirected immediately"));
						return;
					}
					this.#delay = undefined;
					this.#deadline = undefined;
				}
			} catch (err) {
				// Treat teardown as cancellation, not a connection failure.
				if (signal.aborted) return;

				console.warn("connection error:", err);
				this.#retry(effect, connected, err);
			}
		});
	}

	/**
	 * Act on the peer's GOAWAY for `connection`, which was dialed at `dialing`: resolve where
	 * to go next and leave the old session serving until it drains. Returns false when the
	 * redirect is refused, which ends the sequence rather than redialing.
	 */
	#migrate(connection: Established, dialing: URL, drain: Drain): boolean {
		const hashes = this.webtransport?.serverCertificateHashes?.length ?? 0;
		const configured = hashes > 0 || this.webtransport?.serverCertificate !== undefined;
		// The pin is a WebTransport option. A WebSocket that won the race never used it.
		const pinned = pinnedTransport(connection.transport, configured);

		let next: URL | undefined;
		try {
			next = target(this.goaway.redirect ?? "same-host", drain.uri, dialing, pinned);
		} catch (err) {
			// The peer is leaving and named somewhere we won't go: redialing the old address
			// would ignore it, so stop here.
			console.warn("GOAWAY redirect refused:", err);
			connection.abort();
			this.established.set(undefined);
			this.status.set("disconnected");
			this.#giveUp(error(err));
			return false;
		}

		// Only an accepted redirect replaces the URL; an empty one keeps it.
		if (next) {
			this.#redirect.set(next);
			this.#redirectHref = this.#sequenceHref;
		}

		console.info("GOAWAY received; migrating");
		// A newer GOAWAY retires an older predecessor rather than holding two open.
		this.#draining?.retire();
		const cap = handover(this.goaway.handover ?? DEFAULT_HANDOVER, drain.timeout);
		const draining = new Draining(connection, cap, () => {
			if (this.#draining === draining) this.#draining = undefined;
			// If nothing replaced it yet, nothing is serving.
			if (this.established.peek() !== connection) return;
			this.established.set(undefined);
			this.status.set(this.#dialing ? "connecting" : "disconnected");
		});
		this.#draining = draining;
		return true;
	}

	#initial(): Milli {
		return this.delay?.initial ?? DEFAULT_DELAY.initial;
	}

	/**
	 * Schedule the next connect attempt after the current backoff, or stop once the retry window
	 * has expired. `connected` is when the dead session was established, if it ever was, and
	 * `cause` the error that killed it, if it died with one.
	 */
	#retry(effect: Effect, connected: Instant | undefined, cause?: unknown): void {
		// Resolved per sequence rather than at construction, so an edit to `delay` (including
		// one that drops a field back to its default) applies to the next retry. Field by
		// field rather than by spread: a caller building `{ initial: maybeInitial }` from an
		// optional value passes an explicit undefined, which a spread would take as the
		// answer, turning the backoff into NaN or the window into forever.
		const delay = this.delay ?? {};
		const initial = this.#initial();
		const multiplier = delay.multiplier ?? DEFAULT_DELAY.multiplier;
		const max = delay.max ?? DEFAULT_DELAY.max;
		const timeout = delay.timeout ?? DEFAULT_DELAY.timeout;

		// Report disconnected during the backoff rather than when the retry reruns the
		// effect, unless a drained predecessor still serves until it retires.
		if (this.established.peek() === undefined) this.status.set("disconnected");

		// A session that outlived the initial delay was healthy, so clear the backoff and
		// start a fresh retry window: a one-off drop should reconnect promptly. Anything
		// shorter is a peer that accepts and immediately severs, which has to keep
		// escalating or we hammer it forever at the initial delay.
		if (connected !== undefined && Instant.durationSince(now(), connected) >= Nano.fromMilli(initial)) {
			this.#delay = undefined;
			this.#deadline = undefined;
		}

		// An auth rejection stops this URL however long the session lived. UNAUTHORIZED and
		// EXPIRED_AUTH_TOKEN are specified codes rather than ones we guessed at, so this is the
		// peer saying these credentials will never work again; retrying them just burns the
		// window, and an expired token needs the app to refresh it via a new URL. Matches
		// moq-tokio's reconnect loop, which stops on the same close. A new URL or a
		// disable/re-enable starts another sequence; the handle itself is not disposed.
		//
		// Only a session close says that. The stream registry gives 2 to DELIVERY_TIMEOUT,
		// so a stream reset during the SETUP exchange would otherwise suppress reconnect
		// for good.
		if (
			cause instanceof SessionError &&
			(cause.code === SessionCode.Unauthorized || cause.code === SessionCode.Expired)
		) {
			console.warn("session rejected as unauthorized, not retrying");
			this.#giveUp(cause);
			return;
		}

		const at = now();
		this.#delay ??= initial;
		this.#deadline ??= timeout > 0 ? Instant.add(at, Nano.fromMilli(timeout)) : Number.POSITIVE_INFINITY;

		if (at >= this.#deadline) {
			console.warn("reconnect timed out");
			// A graceful close has no error, so report the timeout itself.
			this.#giveUp(cause === undefined ? new Error("reconnect timed out") : error(cause));
			return;
		}

		// Equal jitter, so a fleet of tabs knocked offline together doesn't reconnect on the same
		// tick, and never past the deadline the retry window promised.
		const wait = Math.min(Nano.fromMilli(this.#delay) * (0.5 + Math.random() / 2), this.#deadline - at);
		this.#delay = Milli(Math.min(this.#delay * multiplier, max));

		const tick = this.#tick.peek() + 1;
		effect.timer(() => this.#tick.update((prev) => Math.max(prev, tick)), Nano(wait));
	}

	#giveUp(cause: Error): void {
		this.#error.set(cause);
		this.#givenUpHref = this.#sequenceHref;
		this.#resetSequence();
	}

	#resetSequence(): void {
		this.#delay = undefined;
		this.#deadline = undefined;
		this.#sequenceHref = undefined;
		this.#draining?.retire();
	}

	/**
	 * Subscribe to broadcast announcements matching `scope`, spanning reconnects.
	 *
	 * The same {@link Announce.Consumer} stream as {@link Established.announced}, but everything active
	 * is retracted (an `end` event) whenever the connection drops and re-announced on
	 * reconnect, so a consumer draining `next()` never clings to a dead route across a reconnect.
	 */
	announced(scope: Path.Pattern = Path.Pattern.all(), options?: Announce.Options): Announce.Consumer {
		// With a consume origin the table already spans reconnects (the forwarder retracts
		// a dead session's entries), so its stream is the same thing with less machinery.
		if (this.consume) return this.consume.announced(scope, options);

		const producer = new Announce.Producer();
		const consumer = producer.consume();

		const pump = new Effect();
		pump.run((effect) => {
			const conn = effect.get(this.established);
			if (!conn) return;

			const upstream = conn.announced(scope, options);
			effect.cleanup(() => upstream.close());

			// Track what this connection announced so we can retract it if the connection
			// drops; the last event rides along for the retraction.
			const active = new Map<Path.Valid, Announce.Announce>();

			effect.spawn(async () => {
				try {
					for (;;) {
						const entry = await effect.race(upstream.next());
						if (!entry) break;
						if (entry.kind === "end") active.delete(entry.prefix);
						else active.set(entry.prefix, entry);
						producer.append(entry);
					}
				} catch {
					// A dropped connection resets the announce stream; the retractions below cover it.
				} finally {
					// Retract everything from the connection that just went away, so a per-broadcast
					// watcher tears down instead of clinging to the dead route.
					if (consumer.closed.peek() === undefined) {
						for (const entry of active.values()) {
							producer.append({ ...entry, kind: "end" });
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
	 * Snapshot the live connection's transport counters, or undefined while disconnected.
	 * See {@link Established.stats}.
	 */
	async stats(): Promise<Stats | undefined> {
		return this.established.peek()?.stats();
	}

	/** Stop reconnecting, close the current connection, and settle {@link Reload.closed}. Idempotent. */
	close(abort?: Error) {
		this.#signals.close();
		this.#draining?.retire();
		if (this.#closed.peek() === undefined) this.#closed.set(abort ?? null);
	}
}

/**
 * A session the peer sent GOAWAY on, left serving so its groups in flight finish. It retires
 * when it closes on its own or overstays its handover window, and `onRetire` runs once either way.
 */
class Draining {
	#connection: Established;
	#timer = new AbortController();
	#onRetire: () => void;
	#retired = false;

	constructor(connection: Established, handover: Milli, onRetire: () => void) {
		this.#connection = connection;
		this.#onRetire = onRetire;
		void sleep(Nano.fromMilli(handover), { signal: this.#timer.signal }).then(
			() => {
				if (this.#retired) return;
				console.warn("old session did not drain in time; closing");
				this.retire();
			},
			() => {},
		);
		void connection.closed.then(() => this.retire());
	}

	/** Close the old session now, whatever remains of its window. Idempotent. */
	retire(): void {
		if (this.#retired) return;
		this.#retired = true;
		this.#timer.abort();
		this.#connection.abort();
		this.#onRetire();
	}
}
