/**
 * A reconnecting, shareable connection: one origin and one reconnect loop per relay URL.
 *
 * @module
 */
import { Effect, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import * as Announce from "../announced.ts";
import type { Handle } from "../bandwidth.ts";
import * as Origin from "../origin.ts";
import * as Path from "../path.ts";
import { type AcceptProps as AcceptPropsType, accept } from "./accept.ts";
import { isWebTransportSupported } from "./browser.ts";
import {
	type CertificateHash as CertificateHashType,
	type ConnectProps as ConnectPropsType,
	certificateHash,
	connect,
	type WebSocketOptions as WebSocketOptionsType,
	type WebTransportProps as WebTransportPropsType,
} from "./connect.ts";
import type { Established } from "./established.ts";
import { Reload, type ReloadDelay, type ReloadStatus } from "./reload.ts";
import type { Probe as ProbeType, Stats as StatsType } from "./stats.ts";
import type { Transport as TransportType } from "./transport.ts";

/** How long an unreferenced shared connection lingers before it actually closes. */
const LINGER_MS = 2000;

/** Options for {@link Connection}. */
export interface ConnectionProps {
	/** The relay to connect to; pass a `Signal` to switch relays live. */
	url?: URL | Signal<URL | undefined>;

	/** Whether to connect at all (default: true); pass a `Signal` to toggle live. */
	enabled?: boolean | Signal<boolean>;

	/**
	 * How long the underlying connection outlives its last handle, in milliseconds
	 * (default: 2000).
	 *
	 * The window is what makes moving an element around the DOM free: the connection and
	 * everything it discovered are still warm when the new owner asks for them. Applied by
	 * whoever dials first, so a later handle sharing the connection inherits it.
	 */
	linger?: DOMHighResTimeStamp;

	/**
	 * Share a pooled transport keyed on the URL (default: true).
	 *
	 * Options the pool cannot honor (transport options, discovery, delay, a pinned
	 * certificate, caller-owned origins) require `share: false` and get a private
	 * reconnect loop with the same handle semantics.
	 */
	share?: boolean;

	/** WebTransport options applied to each connection attempt (not reactive). */
	webtransport?: WebTransportPropsType;

	/** WebSocket fallback options applied to each connection attempt (not reactive). */
	websocket?: WebSocketOptionsType;

	/** Whether the relay supports broadcast discovery. */
	discovery?: boolean;

	/** The origin whose broadcasts are served, spanning reconnects. Requires {@link ConnectionProps.share} `false`. */
	publish?: Origin.Consumer;

	/** The origin fed with the peer's announced broadcasts, spanning reconnects. Requires {@link ConnectionProps.share} `false`. */
	subscribe?: Origin.Producer;

	/** Backoff settings for the reconnect loop; an unset field uses its default. */
	delay?: ReloadDelay;

	/** A Connection owns the abort signal for each connection attempt. */
	signal?: never;

	/** A one-shot transport cannot be reused by the reconnect loop; call {@link Connection.connect} instead. */
	transport?: never;
}

/**
 * A cloneable handle on a MoQ connection: `new Connection({ url })` reconnects and, by
 * default, shares one origin and one reconnect loop with every other handle on that URL.
 *
 * The shared {@link origin} is wired to both directions. Everything the relay announces
 * lands in it, a publish into it is announced to the relay, and a page that publishes and
 * watches the same path resolves it locally with no round trip.
 *
 * {@link close} releases this handle; the connection survives its last handle by a short
 * linger window (see {@link ConnectionProps.linger}), so a component torn down and rebuilt
 * reuses the warm connection instead of redialing.
 *
 * The loop reconnects for as long as a handle holds it, so an outage of any length recovers
 * on its own. An auth rejection stops retrying those credentials and retires the shared
 * connection so the next handle dials fresh; a new URL on this handle starts another
 * sequence. {@link closed} settles only when this handle is released.
 *
 * Options the pool cannot honor (transport options, discovery, delay, a pinned
 * certificate, caller-owned origins) take `share: false` and get a private loop. A
 * supplied transport cannot reconnect at all; pass it to {@link Connection.connect}
 * instead.
 *
 * @public
 */
export class Connection {
	/** Establish a one-shot session; a supplied transport belongs here, not in the reconnect loop. */
	static readonly connect = connect;

	/** Accept a one-shot session on an already-open transport. */
	static readonly accept = accept;

	/** SHA-256 of a certificate, for pinning via `webtransport.serverCertificateHashes`. */
	static readonly certificateHash = certificateHash;

	/** Whether this runtime can connect with WebTransport. */
	static readonly isWebTransportSupported = isWebTransportSupported;

	/** Relay URL to connect to; updating it reconnects to that URL. */
	url: Signal<URL | undefined>;

	/** Whether to hold a connection at all; clearing it releases this handle's share. */
	enabled: Signal<boolean>;

	/** Current status of the connection. */
	readonly status: Getter<ReloadStatus>;

	/**
	 * The failure that stopped retrying the current URL, or undefined while the loop is live.
	 *
	 * Set on an auth rejection or when a private retry window expires. Cleared when a new URL
	 * or a disable/re-enable starts another sequence, not on a page hide/show.
	 */
	readonly error: Getter<Error | undefined>;

	/**
	 * The wire transport the current session runs over, or undefined while disconnected.
	 *
	 * The session itself is deliberately not exposed: it is shared, so no handle may close
	 * or reconfigure it, and everything else it offers is reachable through {@link origin}.
	 */
	readonly transport: Getter<TransportType | undefined>;

	/** The current connection's PROBE estimates, or undefined while disconnected. */
	readonly probe: Getter<ProbeType | undefined>;

	/**
	 * The send-side bandwidth allocator for the current URL, or undefined
	 * while disabled or URL-less.
	 *
	 * Every handle on this URL shares the same instance, so publishers reserve
	 * against one registry. Borrowed, not owned: the type has no close, since
	 * closing it would starve every other handle.
	 */
	readonly bandwidth: Getter<Handle | undefined>;

	/**
	 * The origin for the current URL, or undefined while disabled or URL-less.
	 *
	 * Publish into it or consume from it; a shared handle's origin is the same one every
	 * other handle on this URL uses, and it spans the connection's reconnects. Borrowed,
	 * not owned: the type has no close, since closing it would tear the origin down under
	 * every other handle.
	 */
	readonly origin: Getter<Origin.Table | undefined>;

	/**
	 * Settles once this handle is released via {@link close}: `null` on a clean close, or
	 * the abort {@link Error}. Peek it synchronously (`undefined` while open), observe it
	 * reactively, or `await` it. Attempt failures live on {@link error} instead.
	 */
	get closed(): GetPromise<Error | null> {
		return this.#closed;
	}

	#closed = new Once<Error | null>();
	readonly #status = new Signal<ReloadStatus>("disconnected");
	readonly #established = new Signal<Established | undefined>(undefined);
	readonly #probe = new Signal<ProbeType | undefined>(undefined);
	readonly #origin = new Signal<Origin.Producer | undefined>(undefined);
	readonly #bandwidth = new Signal<Handle | undefined>(undefined);
	readonly #error = new Signal<Error | undefined>(undefined);
	#signals = new Effect();

	/**
	 * Take a handle on the connection for {@link ConnectionProps.url}.
	 *
	 * Dials immediately when a URL is given and `enabled` is not false; otherwise waits for
	 * the signals to say go. The handle owns nothing but its own share: {@link close}
	 * releases it, and the underlying connection and origin live for as long as any handle
	 * (plus the linger window) wants them.
	 */
	constructor(props?: ConnectionProps) {
		refuse(props);

		this.url = Signal.from(props?.url);
		this.enabled = Signal.from(props?.enabled ?? true);
		this.status = this.#status;
		this.error = this.#error;
		this.probe = this.#probe;
		this.origin = this.#origin;
		this.bandwidth = this.#bandwidth;
		this.transport = this.#signals.computed((effect) => effect.get(this.#established)?.transport);

		const linger = props?.linger;

		// Key on the serialized URL: URL objects use identity equality, and an equivalent
		// instance must not release and redial.
		const href = this.#signals.computed((effect) => effect.get(this.url)?.href);

		if (props?.share === false) {
			this.#runPrivate(props, href);
			return;
		}

		this.#signals.run((effect) => {
			if (!effect.get(this.enabled)) return;
			const key = effect.get(href);
			if (!key) return;

			const lease = acquire(key, linger);
			effect.cleanup(lease.release);

			effect.set(this.#origin, lease.origin, undefined);
			effect.set(this.#bandwidth, lease.connection.bandwidth, undefined);

			// Proxy the shared loop's outputs, so this handle reads like its own connection.
			effect.run((nested) => nested.set(this.#status, nested.get(lease.connection.status), "disconnected"));
			effect.run((nested) => nested.set(this.#established, nested.get(lease.connection.established), undefined));
			effect.run((nested) => nested.set(this.#probe, nested.get(lease.connection.probe), undefined));
			effect.run((nested) => nested.set(this.#error, nested.get(lease.connection.error), undefined));
		});
	}

	#runPrivate(props: ConnectionProps, href: Getter<string | undefined>): void {
		const owned = props.subscribe === undefined;
		const origin = props.subscribe ?? new Origin.Producer();
		if (owned) this.#signals.cleanup(() => origin.close());

		const loop = new Reload({
			url: this.url,
			enabled: this.enabled,
			publish: props.publish ?? (owned ? origin.consume() : undefined),
			subscribe: origin,
			webtransport: props.webtransport,
			websocket: props.websocket,
			discovery: props.discovery,
			// A handle nobody watches wants unlimited retries; an auth rejection still
			// stops this URL, and a new one starts another sequence.
			delay: props.delay ?? { timeout: 0 },
		});
		this.#signals.cleanup(() => loop.close());

		this.#signals.run((effect) => {
			if (!effect.get(this.enabled) || !effect.get(href)) return;
			effect.set(this.#origin, origin, undefined);
			effect.set(this.#bandwidth, loop.bandwidth, undefined);
		});
		this.#signals.run((effect) => effect.set(this.#status, effect.get(loop.status), "disconnected"));
		this.#signals.run((effect) => effect.set(this.#established, effect.get(loop.established), undefined));
		this.#signals.run((effect) => effect.set(this.#probe, effect.get(loop.probe), undefined));
		this.#signals.run((effect) => effect.set(this.#error, effect.get(loop.error), undefined));
	}

	/**
	 * Subscribe to broadcast announcements under an optional prefix, spanning reconnects
	 * and URL switches: a switch retracts everything from the old relay's origin, then the
	 * new one's arrivals stream in.
	 */
	announced(prefix: Path.Valid = Path.empty()): Announce.Consumer {
		const producer = new Announce.Producer(prefix);
		const consumer = producer.consume();

		// Closing the consumer closes the shared state, so stop appending after that.
		let closed = false;
		void consumer.closed.then(() => {
			closed = true;
		});

		// A child of this handle's scope rather than a standalone Effect it merely cleans up
		// after: the disposer run() hands back also drops itself from the parent, so opening
		// and closing announcement streams repeatedly does not pile up dead pumps that live
		// until the whole handle closes.
		const stop = this.#signals.run((effect) => {
			const origin = effect.get(this.#origin);
			if (!origin) return;

			const upstream = origin.announced(prefix);
			effect.cleanup(() => upstream.close());

			// Track what this origin announced so a URL switch retracts it.
			const active = new Set<Path.Valid>();

			effect.spawn(async () => {
				try {
					for (;;) {
						const entry = await Promise.race([effect.cancel, upstream.next()]);
						if (!entry) break;
						if (entry.active) active.add(entry.prefix);
						else active.delete(entry.prefix);
						producer.append(entry);
					}
				} finally {
					if (!closed) {
						for (const path of active) {
							producer.append({ prefix: path, active: false });
						}
					}
				}
			});
		});

		void consumer.closed.then(stop);

		return consumer;
	}

	/**
	 * A reactive handle to one broadcast on the connection's origin; see `Announce.Broadcast`.
	 * Close the handle when done.
	 */
	announcedBroadcast(path: Path.Valid): Announce.Broadcast {
		// The signal is handed out directly: Producer implements the non-owning Table, so
		// the handle can read the origin but never close it.
		return new Announce.Broadcast({ origin: this.#origin, path });
	}

	/** Snapshot the live connection's transport counters, or undefined while disconnected. */
	async stats(): Promise<StatsType | undefined> {
		return this.#established.peek()?.stats();
	}

	/**
	 * Release this handle. The shared connection closes once its last handle is gone and
	 * the linger window passes; other handles on the URL are unaffected. Idempotent.
	 */
	close(abort?: Error): void {
		this.#signals.close();
		if (this.#closed.peek() === undefined) this.#closed.set(abort ?? null);
	}
}

/** Types on {@link Connection}: the handle is the class, these are its associated types. */
export namespace Connection {
	/** Options for {@link Connection}. */
	export type Props = ConnectionProps;
	/** Options for {@link Connection.connect}. */
	export type ConnectProps = ConnectPropsType;
	/** Options for {@link Connection.accept}. */
	export type AcceptProps = AcceptPropsType;
	/** Backoff settings for a private reconnect loop. */
	export type Delay = ReloadDelay;
	/** Current state of a {@link Connection}. */
	export type Status = ReloadStatus;
	/** The current connection's PROBE estimates. */
	export type Probe = ProbeType;
	/** A point-in-time snapshot of the transport's counters. */
	export type Stats = StatsType;
	/** The wire transport a session runs over. */
	export type Transport = TransportType;
	/** Tuning for the WebSocket fallback. */
	export type WebSocketOptions = WebSocketOptionsType;
	/** WebTransport options, including friendlier certificate pinning. */
	export type WebTransportProps = WebTransportPropsType;
	/** A server certificate hash used to pin a self-signed server. */
	export type CertificateHash = CertificateHashType;
}

/** Throw if `props` cannot be honored, rather than silently dropping them. */
function refuse(props?: ConnectionProps): void {
	if (!props) return;

	const extra = props as ConnectionProps & { transport?: unknown; signal?: unknown };
	if (extra.transport) {
		throw new Error("a supplied transport cannot reconnect; call Connection.connect() instead");
	}
	if (extra.signal) {
		throw new Error("a Connection owns its abort signal; do not pass one");
	}
	if (props.share === false) return;

	if (props.publish || props.subscribe) {
		throw new Error("caller-owned origins cannot be shared; pass share: false");
	}
	const hashes = props.webtransport?.serverCertificateHashes?.length ?? 0;
	if (props.webtransport?.serverCertificate !== undefined || hashes > 0) {
		throw new Error("a pinned certificate cannot be shared; pass share: false");
	}
	if (props.webtransport !== undefined) {
		throw new Error("webtransport options cannot be shared; pass share: false");
	}
	if (props.websocket !== undefined) {
		throw new Error("websocket options cannot be shared; pass share: false");
	}
	if (props.discovery !== undefined) {
		throw new Error("discovery cannot be shared; pass share: false");
	}
	if (props.delay !== undefined) {
		throw new Error("delay cannot be shared; pass share: false");
	}
}

/** One shared connection and the handles keeping it alive. */
interface Entry {
	origin: Origin.Producer;
	connection: Reload;
	refs: number;
	linger: DOMHighResTimeStamp;
	timer?: ReturnType<typeof setTimeout>;
}

/** The process-wide pool backing {@link Connection}. */
const pool = new Map<string, Entry>();

/** Take a reference on the shared entry for `key`, creating it on first use. */
function acquire(key: string, linger?: DOMHighResTimeStamp): Entry & { release: () => void } {
	let entry = pool.get(key);
	if (!entry) {
		const origin = new Origin.Producer();
		const connection = new Reload({
			url: new URL(key),
			enabled: true,
			publish: origin.consume(),
			subscribe: origin,
			// Nobody observes a shared loop's `closed`, so giving up would strand every handle
			// on this URL offline until the page reloads. Retry for as long as the entry lives
			// instead; an auth rejection still stops this URL, and evicts below.
			delay: { timeout: 0 },
		});

		const created: Entry = { origin, connection, refs: 0, linger: linger ?? LINGER_MS };

		// The loop only stops on a peer saying these credentials will never work. Drop the
		// entry so a later handle dials fresh rather than joining a loop that has stopped;
		// handles already on it keep it until they release, since a redial would be refused
		// the same way.
		connection.error.subscribe((err) => {
			if (err !== undefined && pool.get(key) === created) pool.delete(key);
		});

		entry = created;
		pool.set(key, created);
	}

	const taken = entry;
	taken.refs += 1;
	if (taken.timer !== undefined) {
		clearTimeout(taken.timer);
		taken.timer = undefined;
	}

	let released = false;
	return {
		...taken,
		release: () => {
			if (released) return;
			released = true;

			taken.refs -= 1;
			if (taken.refs > 0) return;

			taken.timer = setTimeout(() => {
				if (pool.get(key) === taken) pool.delete(key);
				taken.connection.close();
				taken.origin.close();
			}, taken.linger);

			// Don't hold a Node process open for a connection nobody is using.
			(taken.timer as { unref?: () => void }).unref?.();
		},
	};
}

/**
 * Close every shared connection immediately, so the next handle dials fresh.
 *
 * Exists for tests, which otherwise share connections across cases.
 *
 * @internal
 */
export function resetShared(): void {
	const entries = [...pool.values()];
	pool.clear();
	for (const entry of entries) {
		if (entry.timer !== undefined) clearTimeout(entry.timer);
		entry.connection.close();
		entry.origin.close();
	}
}
