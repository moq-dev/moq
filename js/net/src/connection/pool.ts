import type { Getter } from "@moq/signals";
import * as announce from "../announced.ts";
import type * as broadcast from "../broadcast.ts";
import type * as Path from "../path.ts";
import type { ConnectProps } from "./connect.ts";
import type { Established } from "./established.ts";
import type { Probe, Stats } from "./stats.ts";
import type { Transport } from "./transport.ts";

/** How long an unreferenced session lingers before it actually closes. */
const DEFAULT_GRACE_MS = 2000;

/** Tuning for the shared session pool; see `pool` on the connect options. */
export interface PoolProps {
	/**
	 * How long an unreferenced session lingers before closing, in milliseconds (default: 2000).
	 *
	 * The window is what makes moving an element around the DOM free: the session is still
	 * warm when the new owner asks for it. Applied by whoever opens the session, so a later
	 * caller sharing it inherits the original window.
	 */
	grace?: DOMHighResTimeStamp;
}

/**
 * The pool key for a connection, or `undefined` when it must not be shared.
 *
 * Keyed on the full URL because auth tokens ride in the query string, plus everything else
 * that shapes the session. Options that can't be compared safely opt out entirely rather
 * than risk sharing a session that doesn't match what the caller asked for.
 *
 * @internal
 */
export function poolKey(url: URL, props: ConnectProps | undefined, discovery: boolean): string | undefined {
	if (props?.pool === false) return undefined;

	// The caller owns this session; we can't take it over and hand it to someone else.
	if (props?.transport) return undefined;

	// Certificate pins are `BufferSource | string`, which no fingerprint compares honestly.
	// Sharing a session pinned to the wrong certificate is not a failure worth risking.
	const webtransport = props?.webtransport;
	if (webtransport?.serverCertificate !== undefined) return undefined;
	if (webtransport?.serverCertificateHashes !== undefined) return undefined;

	const websocket = props?.websocket;
	const fingerprint = JSON.stringify([
		discovery,
		webtransport ?? null,
		websocket?.enabled ?? null,
		websocket?.url?.href ?? null,
		websocket?.delay ?? null,
	]);

	// NUL can't appear in a URL, so the two halves can't run together.
	return `${url.href}\0${fingerprint}`;
}

/**
 * One shared session, plus the handles that keep it alive.
 *
 * @internal
 */
export class Entry {
	/** Aborts the shared connection attempt once nobody is waiting on it. */
	readonly signal: AbortSignal;

	/** True once the session died, so the pool stops handing it out. */
	dead = false;

	#pool: Pool;
	#key: string;
	#grace: DOMHighResTimeStamp;
	#controller = new AbortController();

	// The dial, then the session it produced. `session` is what makes an entry reusable
	// without awaiting, and what tells a release whether there is anything warm to keep.
	#pending?: Promise<Established>;
	#session?: Established;

	#refs = 0;
	#timer?: ReturnType<typeof setTimeout>;

	constructor(pool: Pool, key: string, grace: DOMHighResTimeStamp) {
		this.#pool = pool;
		this.#key = key;
		this.#grace = grace;
		this.signal = this.#controller.signal;
	}

	/** Hand the entry the connection attempt made with its {@link signal}. */
	settle(pending: Promise<Established>): void {
		this.#pending = pending;

		void pending.then(
			(session) => {
				this.#session = session;

				// Read `closed` once: it derives a fresh promise per access, and it rejects on an
				// abnormal close, so a one-argument `then` would both leak an unhandled rejection
				// and leave the dead session in the map for the next caller to lease.
				const died = () => this.#died();
				void session.closed.then(died, died);

				// The last holder left while this was still in flight, so nothing wants it now.
				if (this.#refs === 0) session.close();
			},
			() => this.#evict(),
		);
	}

	/**
	 * Take a handle on the session, waiting if the attempt is still in flight.
	 *
	 * `signal` aborts this caller's wait only. The shared attempt survives as long as anyone
	 * else is still waiting on it.
	 */
	async acquire(signal?: AbortSignal): Promise<Established> {
		// Claim the reference before awaiting, so a release in between doesn't tear down the
		// entry underneath us.
		this.#refs += 1;
		this.#cancelGrace();

		try {
			signal?.throwIfAborted();

			const session = this.#session ?? (await abortable(this.#pending, signal));
			if (this.dead) throw new Error("connection closed");

			return new Lease(this, session);
		} catch (err) {
			this.release();
			throw err;
		}
	}

	/** Drop a handle, closing the session once the grace period expires with none left. */
	release(): void {
		this.#refs -= 1;
		// Only the handle that takes the count to zero decides what happens next.
		if (this.#refs !== 0) return;

		if (this.dead) {
			this.#evict();
			return;
		}

		const session = this.#session;
		if (!session) {
			// Nothing warm to preserve yet, so don't hold a half-open dial. Evict first: the
			// aborted attempt is doomed, and a caller arriving in this same turn has to start a
			// fresh one rather than join it.
			this.#evict();
			this.#controller.abort();
			return;
		}

		this.#timer = setTimeout(() => {
			this.#evict();
			session.close();
		}, this.#grace);

		// Don't hold a Node process open for a session nobody is using.
		(this.#timer as { unref?: () => void }).unref?.();
	}

	/** Close the session and drop the entry, whoever still holds a handle. */
	close(): void {
		// Dead, so a handle released afterwards doesn't schedule a second teardown.
		this.dead = true;
		this.#cancelGrace();
		this.#evict();
		this.#controller.abort();
		this.#session?.close();
	}

	#died(): void {
		this.dead = true;
		this.#cancelGrace();
		this.#evict();
	}

	#cancelGrace(): void {
		if (this.#timer === undefined) return;
		clearTimeout(this.#timer);
		this.#timer = undefined;
	}

	#evict(): void {
		this.#pool.evict(this.#key, this);
	}
}

/** Reject as soon as `signal` aborts, without disturbing `pending`. */
async function abortable<T>(pending: Promise<T> | undefined, signal?: AbortSignal): Promise<T> {
	if (!pending) throw new Error("connection attempt never started");
	if (!signal) return await pending;

	const { promise: aborted, reject } = Promise.withResolvers<never>();
	const onAbort = () => reject(signal.reason);
	signal.addEventListener("abort", onAbort, { once: true });

	try {
		return await Promise.race([pending, aborted]);
	} finally {
		signal.removeEventListener("abort", onAbort);
	}
}

/**
 * A reference-counted handle on a shared session.
 *
 * Everything is the underlying session's, except {@link close}, which releases this handle
 * rather than terminating the session: the session goes away once every holder has let go.
 * The same shape as {@link broadcast.Consumer.clone}.
 */
class Lease implements Established {
	readonly url: URL;
	readonly version: string;
	readonly transport: Transport;
	readonly probe: Getter<Probe>;
	readonly discovery: boolean;

	#entry: Entry;
	#session: Established;
	#closed = false;

	// What this handle handed out, so releasing it doesn't leave subscriptions running on a
	// session that outlives it.
	#announced = new Set<announce.Consumer>();
	#consumed = new Set<broadcast.Consumer>();
	#broadcasts = new Set<announce.Broadcast>();
	#published = new Set<broadcast.Producer>();

	constructor(entry: Entry, session: Established) {
		this.#entry = entry;
		this.#session = session;

		this.url = session.url;
		this.version = session.version;
		this.transport = session.transport;
		this.probe = session.probe;
		this.discovery = session.discovery;
	}

	announced(prefix?: Path.Valid): announce.Consumer {
		this.#check();

		const consumer = this.#session.announced(prefix);
		this.#announced.add(consumer);
		void consumer.closed.then(() => this.#announced.delete(consumer));
		return consumer;
	}

	publish(path: Path.Valid, producer: broadcast.Producer): void {
		this.#check();

		this.#session.publish(path, producer);
		this.#published.add(producer);
		void producer.closed.then(() => this.#published.delete(producer));
	}

	consume(path: Path.Valid): broadcast.Consumer {
		this.#check();

		const consumer = this.#session.consume(path);
		this.#consumed.add(consumer);
		void consumer.closed.then(() => this.#consumed.delete(consumer));
		return consumer;
	}

	announcedBroadcast(path: Path.Valid): announce.Broadcast {
		this.#check();

		// Built against this handle, not the session, so it consumes through us and goes away
		// with us.
		const watch = new announce.Broadcast({ connection: this, path });
		this.#broadcasts.add(watch);
		return watch;
	}

	async stats(): Promise<Stats> {
		return await this.#session.stats();
	}

	get closed(): Promise<void> {
		return this.#session.closed;
	}

	close(): void {
		if (this.#closed) return;
		this.#closed = true;

		for (const watch of this.#broadcasts) watch.close();
		for (const consumer of this.#consumed) consumer.close();
		for (const consumer of this.#announced) consumer.close();
		for (const producer of this.#published) producer.close();

		this.#broadcasts.clear();
		this.#consumed.clear();
		this.#announced.clear();
		this.#published.clear();

		this.#entry.release();
	}

	#check(): void {
		if (this.#closed) throw new Error("connection released");
	}
}

/**
 * Sessions shared by URL, so N components watching one relay dial it once.
 *
 * @internal
 */
export class Pool {
	#entries = new Map<string, Entry>();

	/** The live entry for `key`, or `undefined` when nothing usable is cached. */
	get(key: string): Entry | undefined {
		const entry = this.#entries.get(key);
		if (entry && !entry.dead) return entry;
		return undefined;
	}

	/**
	 * Cache a new entry for `key`, to be handed the connection attempt made with its
	 * {@link Entry.signal}. Call on a {@link get} miss.
	 */
	insert(key: string, grace: DOMHighResTimeStamp = DEFAULT_GRACE_MS): Entry {
		const entry = new Entry(this, key, grace);
		this.#entries.set(key, entry);
		return entry;
	}

	/** Drop `entry` from the cache, unless a newer one already replaced it. */
	evict(key: string, entry: Entry): void {
		if (this.#entries.get(key) === entry) this.#entries.delete(key);
	}

	/** Close every session, whoever still holds a handle. */
	close(): void {
		const entries = [...this.#entries.values()];
		this.#entries.clear();
		for (const entry of entries) entry.close();
	}
}

/** The process-wide pool backing the `pool` connect option. */
const pool = new Pool();

/** @internal */
export function sessionPool(): Pool {
	return pool;
}

/**
 * Close every pooled session, so the next connection dials fresh.
 *
 * Exists for tests, which otherwise share sessions across cases.
 *
 * @internal
 */
export function resetPool(): void {
	pool.close();
}
