/**
 * Shared managed connections: one origin and one reconnect loop per relay URL.
 *
 * @module
 */
import { Effect, type Getter, Signal } from "@moq/signals";
import * as Announce from "../announced.ts";
import * as Origin from "../origin.ts";
import * as Path from "../path.ts";
import type { Established } from "./established.ts";
import { Reload, type ReloadStatus } from "./reload.ts";
import type { Probe, Stats } from "./stats.ts";
import type { Transport } from "./transport.ts";

/** How long an unreferenced shared connection lingers before it actually closes. */
const LINGER_MS = 2000;

/** Options for {@link Shared}. */
export interface SharedProps {
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
}

/**
 * A handle on a connection shared by relay URL: every `Shared` pointing at the same URL is
 * backed by one origin and one reconnect loop, so a page full of components dials once.
 *
 * The shared {@link origin} is wired to both directions. Everything the relay announces
 * lands in it, a publish into it is announced to the relay, and a page that publishes and
 * watches the same path resolves it locally with no round trip.
 *
 * {@link close} releases this handle; the connection survives its last handle by a short
 * linger window (see {@link SharedProps.linger}), so a component torn down and rebuilt
 * reuses the warm connection instead of redialing.
 *
 * The loop reconnects for as long as a handle holds it, so an outage of any length recovers
 * on its own. An auth rejection is the one failure it stops on, and it retires the shared
 * connection so the next handle dials fresh.
 *
 * For a connection with options sharing can't honor (a certificate pin, a supplied
 * transport, origins of your own), construct a {@link Reload} directly instead.
 *
 * @public
 */
export class Shared {
	/** Relay URL to connect to; updating it switches to that URL's shared connection. */
	url: Signal<URL | undefined>;

	/** Whether to hold a connection at all; clearing it releases this handle's share. */
	enabled: Signal<boolean>;

	/** Current status of the shared connection. */
	readonly status: Getter<ReloadStatus>;

	/**
	 * The wire transport the current session runs over, or undefined while disconnected.
	 *
	 * The session itself is deliberately not exposed: it is shared, so no handle may close
	 * or reconfigure it, and everything else it offers is reachable through {@link origin}.
	 */
	readonly transport: Getter<Transport | undefined>;

	/** The current connection's PROBE estimates, or undefined while disconnected. */
	readonly probe: Getter<Probe | undefined>;

	/**
	 * The shared origin for the current URL, or undefined while disabled or URL-less.
	 *
	 * Publish into it or consume from it; it is the same origin every other handle on this
	 * URL uses, and it spans the connection's reconnects. Borrowed, not owned: the type has
	 * no close, since closing it would tear the origin down under every other handle.
	 */
	readonly origin: Getter<Origin.Table | undefined>;

	readonly #status = new Signal<ReloadStatus>("disconnected");
	readonly #established = new Signal<Established | undefined>(undefined);
	readonly #probe = new Signal<Probe | undefined>(undefined);
	readonly #origin = new Signal<Origin.Producer | undefined>(undefined);
	#signals = new Effect();

	/**
	 * Take a handle on the shared connection for {@link SharedProps.url}.
	 *
	 * Dials immediately when a URL is given and `enabled` is not false; otherwise waits for
	 * the signals to say go. The handle owns nothing but its own share: {@link close}
	 * releases it, and the underlying connection and origin live for as long as any handle
	 * (plus the linger window) wants them.
	 */
	constructor(props?: SharedProps) {
		this.url = Signal.from(props?.url);
		this.enabled = Signal.from(props?.enabled ?? true);
		this.status = this.#status;
		this.probe = this.#probe;
		this.origin = this.#origin;
		this.transport = this.#signals.computed((effect) => effect.get(this.#established)?.transport);

		const linger = props?.linger;

		// Key on the serialized URL: URL objects use identity equality, and an equivalent
		// instance must not release and redial.
		const href = this.#signals.computed((effect) => effect.get(this.url)?.href);

		this.#signals.run((effect) => {
			if (!effect.get(this.enabled)) return;
			const key = effect.get(href);
			if (!key) return;

			const lease = acquire(key, linger);
			effect.cleanup(lease.release);

			effect.set(this.#origin, lease.origin, undefined);

			// Proxy the shared loop's outputs, so this handle reads like its own connection.
			effect.run((nested) => nested.set(this.#status, nested.get(lease.connection.status), "disconnected"));
			effect.run((nested) => nested.set(this.#established, nested.get(lease.connection.established), undefined));
			effect.run((nested) => nested.set(this.#probe, nested.get(lease.connection.probe), undefined));
		});
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
						if (entry.active) active.add(entry.path);
						else active.delete(entry.path);
						producer.append(entry);
					}
				} finally {
					if (!closed) {
						for (const path of active) {
							producer.append({ path, active: false });
						}
					}
				}
			});
		});

		void consumer.closed.then(stop);

		return consumer;
	}

	/**
	 * A reactive handle to one broadcast on the shared origin; see `Announce.Broadcast`.
	 * Close the handle when done.
	 */
	announcedBroadcast(path: Path.Valid): Announce.Broadcast {
		// The signal is handed out directly: Producer implements the non-owning Table, so
		// the handle can read the origin but never close it.
		return new Announce.Broadcast({ origin: this.#origin, path });
	}

	/** Snapshot the live connection's transport counters, or undefined while disconnected. */
	async stats(): Promise<Stats | undefined> {
		return this.#established.peek()?.stats();
	}

	/**
	 * Release this handle. The shared connection closes once its last handle is gone and
	 * the linger window passes; other handles on the URL are unaffected. Idempotent.
	 */
	close(): void {
		this.#signals.close();
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

/** The process-wide pool backing {@link Shared}. */
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
			// instead; an auth rejection is still terminal, and evicts below.
			delay: { timeout: 0 },
		});

		const created: Entry = { origin, connection, refs: 0, linger: linger ?? LINGER_MS };

		// The loop only stops on a peer saying these credentials will never work. Drop the
		// entry so a later handle dials fresh rather than joining a loop that has stopped;
		// handles already on it keep it until they release, since a redial would be refused
		// the same way.
		void connection.closed.catch(() => {
			if (pool.get(key) === created) pool.delete(key);
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
