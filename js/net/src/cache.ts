/**
 * The idle window of cached groups. Mirrors the idle expiry of the Rust `cache` module, without
 * its byte budget: no JS caller sets a capacity, so nothing here evicts for bytes.
 *
 * Every track of a broadcast caches into one {@link Pool} through its own {@link Track} account,
 * and every cached group records its last access through a {@link Charge}. A group nobody reads
 * or writes for the pool's window is reclaimed, except a live track's latest. Writes scan for
 * idle groups as they go, and the pool sweeps every track at half the window for the ones
 * nothing writes to.
 *
 * Package-internal: not exported from the entrypoint.
 *
 * @module
 */
import { Duration, type Instant, now, sleepUntil } from "@moq/clock";

/** How long a group nobody reads or writes stays cached by default, matching Rust `cache::DEFAULT_EXPIRY`. */
export const DEFAULT_EXPIRY = Duration.millis(30_000);

// The coarse clock behind access stamps, as Rust's: the expiry window rounds up to it.
const TICK = Duration.millis(100);

// Ticks between the idle scans frame writes run, so a low-bitrate track still ages out.
const EXPIRY_SCAN_TICKS = 10;

// Entries one write-driven idle scan examines, few enough that a write never walks a long queue.
const EXPIRY_SCAN = 4;

// Dead entries the scan order tolerates beyond twice the live ones before it is rebuilt.
const ORDER_SLACK = 64;

/** The initial policy for a {@link Pool}. */
export interface Config {
	/** How long a group nobody reads or writes stays cached; never reclaimed for idleness when omitted. */
	expiry?: Duration;
}

/** The idle window shared by the cached groups of every track caching into it. */
export class Pool {
	// The idle window in ticks.
	readonly #expiry?: number;
	// The first instant the pool read, and the latest tick since it, never moving back.
	#epoch?: Instant;
	#tick = 0;
	// The accounts holding something that may expire, which the idle sweep visits.
	#tracks = new Set<Track<Entry>>();
	#sweep?: AbortController;

	constructor(config: Config = {}) {
		if (config.expiry !== undefined) this.#expiry = Math.max(1, Math.ceil(config.expiry.divDuration(TICK)));
	}

	/** The idle window, rounded up to the pool's 100 ms tick, or undefined when idle groups stay. */
	get expiry(): Duration | undefined {
		return this.#expiry === undefined ? undefined : TICK.mul(this.#expiry);
	}

	/** Ticks since the pool first read the clock. */
	now(): number {
		const at = now();
		this.#epoch ??= at;
		this.#tick = Math.max(this.#tick, Math.floor(at.since(this.#epoch).divDuration(TICK)));
		return this.#tick;
	}

	/** The idle window in ticks, or undefined when idle groups stay. */
	get expiryTicks(): number | undefined {
		return this.#expiry;
	}

	/** Visit `track` on each idle sweep, if the pool has an idle window, while it holds something that may expire. */
	register(track: Track<Entry>): void {
		if (this.#expiry === undefined) return;
		this.#tracks.add(track);
		this.#arm();
	}

	/** Stop visiting `track`, which holds nothing that may expire. */
	unregister(track: Track<Entry>): void {
		this.#tracks.delete(track);
	}

	/** Expire the idle groups of every registered track. */
	sweep(): void {
		for (const track of [...this.#tracks]) track.sweep();
	}

	// Sweep again in half the idle window while any track holds something that may expire. Never a
	// reason to hold a Node or Bun process open.
	#arm(): void {
		if (this.#sweep !== undefined || this.#expiry === undefined || this.#tracks.size === 0) return;
		const sweep = new AbortController();
		this.#sweep = sweep;
		const at = now().saturatingAdd(TICK.mul(this.#expiry / 2));
		void sleepUntil(at, { signal: sweep.signal, ref: false }).then(
			() => {
				if (this.#sweep !== sweep) return;
				this.#sweep = undefined;
				this.sweep();
				this.#arm();
			},
			() => {},
		);
	}
}

/** A cached group as its track's account knows it. */
export interface Entry {
	/** Its last access. */
	readonly charge: Charge;
}

// A group's place in an account's scan order, emptied once the group leaves the cache.
interface Slot<E> {
	entry: E | undefined;
}

/** How an account reaches the track whose groups it expires. */
export interface Owner<E extends Entry> {
	/** Whether `entry` is the live edge, which expiry never takes. */
	protects(entry: E): boolean;
	/** Take `entry` out of the cache and end it for idling. */
	evict(entry: E): void;
	/** Something was evicted: settle what the cache's contents decide. */
	settle(): void;
	/** Forget the runs of the drop set given up longer than the pool's idle window before tick `now`, returning whether any went. */
	expireDrops(now: number): boolean;
	/** Whether the track holds a drop run a reclaim may forget, which keeps its account in the idle sweep. */
	holdsDrops(): boolean;
}

/**
 * One track's account against a {@link Pool}: its cached groups, in the order the idle scan
 * visits them. Mirrors the expiry half of the Rust `cache::Track`.
 */
export class Track<E extends Entry> {
	/** The pool this track caches into. */
	readonly pool: Pool;
	#owner: Owner<E>;
	// The tick a frame write may next run an idle scan at.
	#nextExpiry = 0;
	// Every cached group with its slot in the scan order, oldest first. A group leaving the cache
	// empties its slot, so the order never pins its frames; a compaction drops empty slots. The
	// order starts at `#head`, since `Array.shift` moves every slot behind it in V8.
	#live = new Map<E, Slot<E>>();
	#order: Slot<E>[] = [];
	#head = 0;
	// Where the next bounded idle scan starts, rotating so fresh groups at the front do not
	// starve the ones behind them.
	#cursor = 0;

	constructor(pool: Pool, owner: Owner<E>) {
		this.pool = pool;
		this.#owner = owner;
	}

	/** Open the access record of a new group, stamped now. */
	charge(): Charge {
		return makeCharge(this as Track<Entry>);
	}

	/** The drop set changed: keep the account in the idle sweep while it holds a run to forget. */
	dropsChanged(): void {
		if (this.#owner.holdsDrops()) this.pool.register(this as Track<Entry>);
	}

	/** Whether the account caches any group. */
	get empty(): boolean {
		return this.#live.size === 0;
	}

	/** Cache `entry` at the back of the scan order, expirable now unless it is the live edge. */
	insert(entry: E): void {
		const slot = { entry };
		this.#live.set(entry, slot);
		this.#order.push(slot);
		if (!this.#owner.protects(entry)) this.demote(entry);
	}

	/** Make `entry` expirable once the live edge no longer protects it. */
	demote(entry: E): void {
		if (this.#live.has(entry)) this.pool.register(this as Track<Entry>);
	}

	/** Forget `entry`, which left the cache. */
	remove(entry: E): void {
		const slot = this.#live.get(entry);
		if (slot === undefined) return;
		this.#live.delete(entry);
		slot.entry = undefined;
		entry.charge.clear();
		if (this.#live.size === 0) {
			this.#order = [];
			this.#head = 0;
			if (!this.#owner.holdsDrops()) this.pool.unregister(this as Track<Entry>);
		}
	}

	/**
	 * Evict the groups idle past the pool's window, never the live edge: every one when `full`,
	 * or else a bounded scan from a rotating cursor. Returns whether it evicted a group.
	 */
	evictExpired(full: boolean): boolean {
		const expiry = this.pool.expiryTicks;
		const order = this.#order;
		const head = this.#head;
		const len = order.length - head;
		if (expiry === undefined) return false;
		const now = this.pool.now();
		const forgot = this.#owner.expireDrops(now);
		if (len === 0) return forgot;
		const start = full ? 0 : this.#cursor % len;
		const width = full ? len : Math.min(len, EXPIRY_SCAN);
		if (!full) this.#cursor = start + width;
		const expired: E[] = [];
		for (let step = 0; step < width; step++) {
			const entry = order[head + ((start + step) % len)].entry;
			if (entry === undefined || this.#owner.protects(entry)) continue;
			if (now - entry.charge.accessed > expiry) expired.push(entry);
		}
		for (const entry of expired) this.#owner.evict(entry);
		this.#compact();
		return expired.length > 0 || forgot;
	}

	// Skip the empty slots at the front of the order, and rebuild it once the slots before the
	// front and the empty ones behind it clearly outnumber the live ones.
	#compact(): void {
		const order = this.#order;
		while (this.#head < order.length && order[this.#head].entry === undefined) this.#head++;
		if (order.length > 2 * this.#live.size + ORDER_SLACK) {
			this.#order = order.slice(this.#head).filter((slot) => slot.entry !== undefined);
			this.#head = 0;
		}
	}

	/** Settle from a frame write: scan for idle groups once per second, so a track that only appends frames to open groups still ages. */
	settle(): void {
		if (this.#expiryDue() && this.evictExpired(false)) this.#owner.settle();
	}

	/** Expire every idle group, from the pool's sweep. */
	sweep(): void {
		if (this.evictExpired(true)) this.#owner.settle();
		// A live edge alone never expires, so it needs no sweep to keep its track alive for.
		if (this.#owner.holdsDrops()) return;
		for (const entry of this.#live.keys()) if (!this.#owner.protects(entry)) return;
		this.pool.unregister(this as Track<Entry>);
	}

	// Claim the next write-driven idle scan once its time comes.
	#expiryDue(): boolean {
		const expiry = this.pool.expiryTicks;
		if (expiry === undefined) return false;
		const now = this.pool.now();
		if (now < this.#nextExpiry) return false;
		this.#nextExpiry = now + Math.min(expiry, EXPIRY_SCAN_TICKS);
		return true;
	}
}

let makeCharge: (track: Track<Entry>) => Charge;

/**
 * One cached group's last access, which its idle window runs from, and its route to its track's
 * idle scan. Cleared once the group leaves the cache, after which every operation is a no-op.
 */
export class Charge {
	#track?: Track<Entry>;
	#stamp: number;

	private constructor(track: Track<Entry>) {
		this.#track = track;
		this.#stamp = track.pool.now();
	}

	static {
		makeCharge = (track) => new Charge(track);
	}

	/** The tick of the last access. */
	get accessed(): number {
		return this.#stamp;
	}

	/** Record an access, a read or a write, restarting the idle window. */
	touch(): void {
		const track = this.#track;
		if (track) this.#stamp = Math.max(this.#stamp, track.pool.now());
	}

	/** Run the account's idle scan after a frame write. */
	settle(): void {
		this.#track?.settle();
	}

	/** Detach from the account. Idempotent. */
	clear(): void {
		this.#track = undefined;
	}
}
