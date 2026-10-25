/**
 * A reconnecting, shareable connection: one origin and reconnect loop per URL and version list.
 *
 * @module
 */

import { Milli, Nano } from "@moq/clock";
import { Effect, type GetPromise, type Getter, Once, Signal } from "@moq/signals";
import * as Announce from "../announced.ts";
import type * as Auth from "../auth.ts";
import type { Handle } from "../bandwidth.ts";
import * as Origin from "../origin.ts";
import * as Path from "../path.ts";
import { Follow } from "../util/observed.ts";
import { after } from "../util/timer.ts";
import { type AcceptProps as AcceptPropsType, accept } from "./accept.ts";
import { isWebTransportSupported } from "./browser.ts";
import {
	type CertificateHash as CertificateHashType,
	type ConnectProps as ConnectPropsType,
	certificateHash,
	connect,
	normalizeVersions,
	type WebSocketProps as WebSocketPropsType,
	type WebTransportProps as WebTransportPropsType,
} from "./connect.ts";
import type { Established as EstablishedType } from "./established.ts";
import type { Extensions as ExtensionsType } from "./extensions.ts";
import { DISCOVERY_REMOVED } from "./forward.ts";
import type { GoawayProps, Redirect as RedirectType } from "./goaway.ts";
import { pumpAnnounced, Reload, type ReloadDelay, type ReloadStatus } from "./reload.ts";
import type { Probe as ProbeType, Stats as StatsType } from "./stats.ts";
import type { Transport as TransportType } from "./transport.ts";

/** How long an unreferenced shared connection lingers before it actually closes. */
const LINGER_MS = Milli(2000);

/** Options for {@link Connection}. */
export interface ConnectionProps {
	/** The relay to connect to; pass a `Signal` to switch relays live. */
	url?: URL | Signal<URL | undefined>;

	/** Whether to connect at all (default: true); pass a `Signal` to toggle live. */
	enabled?: boolean | Signal<boolean>;

	/**
	 * How long the underlying connection outlives its last handle (default: 2000ms).
	 *
	 * The window is what makes moving an element around the DOM free: the connection and
	 * everything it discovered are still warm when the new owner asks for them. The longest
	 * linger requested by any handle sharing the connection wins.
	 */
	linger?: Milli;

	/**
	 * Share a pooled transport keyed on the URL and ordered versions. By default, options specific to this
	 * handle use a private reconnect loop; otherwise connections share by URL and versions.
	 * Explicit `true` refuses options a pool cannot honor.
	 */
	share?: boolean;

	/**
	 * Protocol versions to offer, most preferred first, spelled like `moq-lite-06`; connections pool by
	 * URL and this list. Pass a `Signal` to switch versions live.
	 */
	versions?: readonly string[] | Signal<readonly string[] | undefined>;

	/** WebTransport options applied to each connection attempt (not reactive). */
	webtransport?: WebTransportPropsType;

	/** WebSocket fallback options applied to each connection attempt (not reactive). */
	websocket?: WebSocketPropsType;

	/** The origin whose broadcasts are served, spanning reconnects. Implies a private loop. */
	publish?: Origin.Consumer;

	/** The origin fed with the peer's announced broadcasts, spanning reconnects. Implies a private loop. */
	consume?: Origin.Producer;

	/** Backoff settings for the reconnect loop; an unset field uses its default. */
	delay?: ReloadDelay;

	/**
	 * Maximum time for each connection attempt, the dial and the MoQ handshake (default:
	 * 30000ms; `0` waits forever). An attempt that outlives it is retried like any other
	 * failure. The same bound as the `timeout` option of {@link Connection.connect}.
	 */
	timeout?: Milli;

	/**
	 * How to react to the relay's GOAWAY; an unset field uses its default. Every connection
	 * migrates on GOAWAY, dialing the replacement while the old session finishes its groups.
	 * This only tunes where it may go and for how long the old session serves.
	 */
	goaway?: GoawayProps;

	/** A Connection owns the abort signal for each connection attempt. */
	signal?: never;

	/** A one-shot transport cannot be reused by the reconnect loop; call {@link Connection.connect} instead. */
	transport?: never;
}

/**
 * A cloneable handle on a MoQ connection: `new Connection({ url })` reconnects and, by
 * default, shares one origin and reconnect loop with handles using the same URL and versions.
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
 * A relay's GOAWAY migrates rather than drops: the replacement dials at once while the old
 * session finishes its groups, and the handle and its origin carry across. A redirect the
 * {@link ConnectionProps.goaway} policy refuses stops the loop like an auth rejection does.
 *
 * Options the pool cannot honor (transport options, delay, timeout, goaway, a pinned
 * certificate, caller-owned origins) use a private loop. An explicit `share: true`
 * refuses them. A supplied transport cannot reconnect at all; pass it to {@link Connection.connect}
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

	/** Protocol versions to offer, or undefined for the defaults; updating it reconnects with that list. */
	versions: Signal<readonly string[] | undefined>;

	/** Whether to hold a connection at all; clearing it releases this handle's share. */
	enabled: Signal<boolean>;

	/** Current status of the connection. */
	readonly status: Getter<ReloadStatus>;

	/**
	 * The failure that stopped retrying the current URL, or undefined while the loop is live.
	 *
	 * Set on an auth rejection, when a private retry window expires, or when {@link versions} names
	 * one this build cannot offer. Cleared when a new URL or version list, or a disable/re-enable,
	 * starts another sequence, not on a page hide/show.
	 */
	readonly error: Getter<Error | undefined>;

	/**
	 * The wire transport the current session runs over, or undefined while disconnected.
	 *
	 * The session itself is deliberately not exposed: it is shared, so no handle may close
	 * or reconfigure it, and everything else it offers is reachable through {@link origin}.
	 */
	readonly transport: Getter<TransportType | undefined>;

	/** Receive estimates while observed, or undefined while disconnected; peek does not request them. */
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
	 * The auth handle every session presents, or undefined while disabled or URL-less.
	 *
	 * Every session the connection makes presents the tokens it holds, so a route naming one
	 * keeps it across a reconnect, and its grant follows the live session. A private
	 * connection keeps one handle across URL changes, so its tokens go to the new URL, and
	 * starts another when `versions` changes. A shared connection's is the one every other
	 * handle on its URL uses, since they share the session.
	 */
	readonly auth: Getter<Auth.Handle | undefined>;

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
	readonly #established = new Signal<EstablishedType | undefined>(undefined);
	readonly #probe = new Signal<Getter<ProbeType | undefined> | undefined>(undefined);
	readonly #origin = new Signal<Origin.Producer | undefined>(undefined);
	readonly #bandwidth = new Signal<Handle | undefined>(undefined);
	readonly #auth = new Signal<Auth.Handle | undefined>(undefined);
	readonly #error = new Signal<Error | undefined>(undefined);
	#signals: Effect;

	/**
	 * Take a handle on the connection for {@link ConnectionProps.url}.
	 *
	 * Dials immediately when a URL is given and `enabled` is not false; otherwise waits for
	 * the signals to say go. The handle owns nothing but its own share: {@link close}
	 * releases it, and the underlying connection and origin live for as long as any handle
	 * (plus the linger window) wants them.
	 */
	constructor(props?: ConnectionProps) {
		const given = props?.versions;
		// A plain list is checked and copied now, so a bad name throws here and mutating the array
		// later changes nothing. A live list is checked as it changes, failing on `error`.
		const fixed = normalizeVersions({ ...props, versions: Array.isArray(given) ? given : undefined });
		// Likewise a bad timeout, before anything below exists to leak.
		if (props?.timeout !== undefined) Milli(props.timeout);
		refuse(props);
		this.#signals = new Effect();

		this.url = Signal.from(props?.url);
		this.versions = Signal.from(Array.isArray(given) ? fixed : given);
		this.enabled = Signal.from(props?.enabled ?? true);
		this.status = this.#status;
		this.error = this.#error;
		this.probe = new Follow(this.#probe);
		this.origin = this.#origin;
		this.bandwidth = this.#bandwidth;
		this.auth = this.#auth;
		this.transport = this.#signals.computed((effect) => effect.get(this.#established)?.transport);

		const linger = props?.linger;

		// Key on the serialized URL: URL objects use identity equality, and an equivalent
		// instance must not release and redial.
		const href = this.#signals.computed((effect) => effect.get(this.url)?.href);

		if (props && (props.share === false || requiresPrivate(props))) {
			this.#runPrivate(props, href);
			return;
		}

		this.#signals.run((effect) => {
			if (!effect.get(this.enabled)) return;
			const key = effect.get(href);
			if (!key) return;
			const versions = offered(effect.get(this.versions));
			if (versions instanceof TypeError) {
				effect.set(this.#error, versions, undefined);
				return;
			}

			const lease = acquire(key, versions, linger);
			effect.cleanup(lease.release);

			effect.set(this.#origin, lease.origin, undefined);
			effect.set(this.#bandwidth, lease.connection.bandwidth, undefined);
			effect.set(this.#auth, lease.connection.auth, undefined);

			// Proxy the shared loop's outputs, so this handle reads like its own connection.
			effect.run((nested) => nested.set(this.#status, nested.get(lease.connection.status), "disconnected"));
			effect.run((nested) => nested.set(this.#established, nested.get(lease.connection.established), undefined));
			effect.run((nested) => nested.set(this.#probe, lease.connection.probe, undefined));
			effect.run((nested) => nested.set(this.#error, nested.get(lease.connection.error), undefined));
		});
	}

	#runPrivate(props: ConnectionProps, href: Getter<string | undefined>): void {
		const owned = props.consume === undefined;
		const origin = props.consume ?? new Origin.Producer();
		if (owned) this.#signals.cleanup(() => origin.close());

		const publish = props.publish ?? (owned ? origin.consume() : undefined);

		// The origin spans version changes; only the loop dialing it is replaced. Each loop is built
		// synchronously, not in an effect a microtask later, so its hold on `consume` is in place
		// before a watcher created alongside could sync on an empty table.
		let stop: (() => void) | undefined;
		const start = (list: readonly string[] | undefined) => {
			stop?.();
			const signals = new Effect();
			stop = () => signals.close();

			const versions = offered(list);
			if (versions instanceof TypeError) {
				signals.run((effect) => effect.set(this.#error, versions, undefined));
				return;
			}

			const loop = new Reload({
				url: this.url,
				enabled: this.enabled,
				publish,
				consume: origin,
				versions,
				webtransport: props.webtransport,
				websocket: props.websocket,
				delay: props.delay,
				timeout: props.timeout,
				goaway: props.goaway,
			});
			signals.cleanup(() => loop.close());

			signals.run((effect) => {
				if (!effect.get(this.enabled) || !effect.get(href)) return;
				effect.set(this.#origin, origin, undefined);
				effect.set(this.#bandwidth, loop.bandwidth, undefined);
				effect.set(this.#auth, loop.auth, undefined);
			});
			signals.run((effect) => effect.set(this.#status, effect.get(loop.status), "disconnected"));
			signals.run((effect) => effect.set(this.#established, effect.get(loop.established), undefined));
			signals.run((effect) => effect.set(this.#probe, loop.probe, undefined));
			signals.run((effect) => effect.set(this.#error, effect.get(loop.error), undefined));
		};
		start(this.versions.peek());
		this.#signals.cleanup(this.versions.subscribe(start));
		this.#signals.cleanup(() => stop?.());
	}

	/**
	 * Subscribe to broadcast announcements matching `scope`, spanning reconnects
	 * and URL switches: a switch retracts everything from
	 * the old relay's origin, then the new one's arrivals stream in.
	 */
	announced(scope: Path.Pattern = Path.Pattern.all(), options?: Announce.Options): Announce.Consumer {
		const producer = new Announce.Producer();
		const consumer = producer.consume();

		// A child of this handle's scope rather than a standalone Effect it merely cleans up
		// after: the disposer run() hands back also drops itself from the parent, so opening
		// and closing announcement streams repeatedly does not pile up dead pumps that live
		// until the whole handle closes.
		const stop = this.#signals.run((effect) => {
			const origin = effect.get(this.#origin);
			if (!origin) {
				// Like a reconnecting loop's origin: behind while enabled with nothing to deliver.
				if (effect.get(this.enabled)) producer.behind();
				else producer.synced();
				return;
			}
			// A URL switch retracts what the old origin announced.
			pumpAnnounced(effect, origin.announced(scope, options), producer);
		});

		void consumer.closed.then(stop);

		return consumer;
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
	export type Backoff = ReloadDelay;
	/** How a connection reacts to the relay's GOAWAY. */
	export type Goaway = GoawayProps;
	/** What to do with the URI a relay names in its GOAWAY. */
	export type Redirect = RedirectType;
	/** Current state of a {@link Connection}. */
	export type Status = ReloadStatus;
	/** The current connection's PROBE estimates. */
	export type Probe = ProbeType;
	/** A point-in-time snapshot of the transport's counters. */
	export type Stats = StatsType;
	/** The wire transport a session runs over. */
	export type Transport = TransportType;
	/** Tuning for the WebSocket fallback. */
	export type WebSocketProps = WebSocketPropsType;
	/** WebTransport options, including friendlier certificate pinning. */
	export type WebTransportProps = WebTransportPropsType;
	/** A server certificate hash used to pin a self-signed server. */
	export type CertificateHash = CertificateHashType;
	/** An established one-shot session. */
	export type Established = EstablishedType;
	/** The moq-transport extensions a one-shot session offers. */
	export type Extensions = ExtensionsType;
}

/** The checked copy of `versions` to dial with, or why it cannot be offered. */
function offered(versions: readonly string[] | undefined): string[] | undefined | TypeError {
	try {
		return normalizeVersions({ versions });
	} catch (err) {
		if (err instanceof TypeError) return err;
		throw err;
	}
}

/** Throw if `props` cannot be honored, rather than silently dropping them. */
function refuse(props?: ConnectionProps): void {
	if (!props) return;

	const extra = props as ConnectionProps & { transport?: unknown; signal?: unknown };
	if ("discovery" in props) throw new TypeError(DISCOVERY_REMOVED);
	if (extra.transport) {
		throw new Error("a supplied transport cannot reconnect; call Connection.connect() instead");
	}
	if (extra.signal) {
		throw new Error("a Connection owns its abort signal; do not pass one");
	}
	if (props.share !== true || !requiresPrivate(props)) return;

	if (props.publish || props.consume) {
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
	if (props.delay !== undefined) {
		throw new Error("delay cannot be shared; pass share: false");
	}
	if (props.timeout !== undefined) {
		throw new Error("timeout cannot be shared; pass share: false");
	}
	if (props.goaway !== undefined) {
		throw new Error("goaway cannot be shared; pass share: false");
	}
}

/** Options tied to one handle cannot be represented by a URL-keyed shared entry. */
function requiresPrivate(props: ConnectionProps): boolean {
	return (
		props.webtransport !== undefined ||
		props.websocket !== undefined ||
		props.delay !== undefined ||
		props.timeout !== undefined ||
		props.goaway !== undefined ||
		props.publish !== undefined ||
		props.consume !== undefined
	);
}

/** One shared connection and the handles keeping it alive. */
interface Entry {
	/** The pool key: the dialed URL and versions, moved together on GOAWAY. */
	key: string;
	origin: Origin.Producer;
	connection: Reload;
	refs: number;
	linger: Milli;
	timer?: () => void;
}

/** The process-wide pool backing {@link Connection}. */
const pool = new Map<string, Entry>();

// Preserve the ordered versions when a redirect moves the URL.
function poolKey(url: string, versions?: readonly string[]): string {
	return JSON.stringify([url, versions]);
}

/** Take a reference on the shared entry, creating it on first use. */
function acquire(url: string, versions?: readonly string[], linger?: Milli): Entry & { release: () => void } {
	const key = poolKey(url, versions);
	let entry = pool.get(key);
	if (!entry) {
		const origin = new Origin.Producer();
		const connection = new Reload({
			url: new URL(url),
			versions,
			enabled: true,
			publish: origin.consume(),
			consume: origin,
		});

		const created: Entry = { key, origin, connection, refs: 0, linger: linger ?? LINGER_MS };

		// The loop only stops on a peer saying these credentials will never work, or on a
		// GOAWAY redirect it refused. Drop the entry so a later handle dials fresh rather
		// than joining a loop that has stopped; handles already on it keep it until they
		// release.
		connection.error.subscribe((err) => {
			if (err !== undefined && pool.get(created.key) === created) pool.delete(created.key);
		});

		// An accepted redirect moves the entry to the URL it now dials, so a later handle
		// configured with that URL shares it, and one asking for the old URL dials fresh.
		// A live entry already at the target wins: this one leaves the pool and serves only
		// the handles it has until they release it.
		connection.redirect.subscribe((redirect) => {
			if (!redirect) return;
			const key = poolKey(redirect.href, versions);
			if (key === created.key) return;
			if (pool.get(created.key) === created) pool.delete(created.key);
			created.key = key;
			if (!pool.has(created.key)) pool.set(created.key, created);
		});

		entry = created;
		pool.set(key, created);
	}

	const taken = entry;
	taken.linger = Milli(Math.max(taken.linger, linger ?? LINGER_MS));
	taken.refs += 1;
	if (taken.timer !== undefined) {
		taken.timer();
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

			// Don't hold a Node process open for a connection nobody is using.
			taken.timer = after(
				Nano.fromMilli(taken.linger),
				() => {
					if (pool.get(taken.key) === taken) pool.delete(taken.key);
					taken.connection.close();
					taken.origin.close();
				},
				{ ref: false },
			);
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
		entry.timer?.();
		entry.connection.close();
		entry.origin.close();
	}
}
