/**
 * A shared byte budget for cached groups, repaid by write-time eviction. Mirrors the Rust
 * `cache` module.
 *
 * Every cached group bills its frames and a fixed overhead through a {@link Charge} to its
 * track's {@link Track} account, and every track of a broadcast caches into one {@link Pool}.
 * The pool never evicts on its own: while it is over capacity, each track accrues eviction debt
 * as it writes, sized to what it wrote, and pays it by evicting its own oldest groups. A group
 * accessed more recently than the mean access of the evictable groups is never evicted, so fresh
 * content in one track outlives stale content in another, and a track whose oldest group is
 * staler than the mean accrues at double rate.
 *
 * The pool also owns the idle window: a group nobody reads or writes for that long is reclaimed,
 * except a live track's latest. Writes scan for idle groups as they go, and the pool sweeps
 * every track at half the window for the ones nothing writes to.
 *
 * Package-internal: not exported from the entrypoint.
 *
 * @module
 */
import { Duration, type Instant, now, sleepUntil } from "@moq/clock";

/** How long a group nobody reads or writes stays cached by default, matching Rust `cache::DEFAULT_EXPIRY`. */
export const DEFAULT_EXPIRY = Duration.millis(30_000);

// Bun 1.4's heap (bun:jsc `heapStats`) grows about 2.9 KB per cached single-frame group with no
// subscriber, and 3.9 KB more per subscriber, whose sink holds its own mirror of every cached
// group. Billing payload alone would let a track of tiny groups cache far more than its budget.
/** Bookkeeping billed per cached group on top of its frames. */
export const ENTRY_OVERHEAD = 3 * 1024;
/** Bookkeeping billed per subscriber mirror a cached group holds. */
export const MIRROR_OVERHEAD = 4 * 1024;
// Bun 1.4's heap grows about 334 bytes per disjoint run of a track's drop set, its change-log
// entry included, and about 53 more per entry of the expiry index a pool with an idle window keeps.
/** Bookkeeping billed per run of a track's drop set. */
export const RUN_OVERHEAD = 336;
/** Bookkeeping billed per entry of a drop set's expiry index. */
export const BORN_OVERHEAD = 64;

// The coarse clock behind access stamps, as Rust's: the expiry window rounds up to it.
const TICK = Duration.millis(100);

// Sub-tick boosts break ties within one tick: a write outranks a merely inserted group, and a
// read outranks both, so a same-tick access still reads as newer than the mean of weaker ones.
const WRITE_BOOST = 1;
const READ_BOOST = 2;
const BOOSTS = 4;

// Bytes a track writes before a frame write settles its debt, so a track appending frames to
// open groups, never inserting another, still pays.
const WRITE_CHARGE_THRESHOLD = 256 * 1024;

// Ticks between the idle scans frame writes run, so a low-bitrate track still ages out.
const EXPIRY_SCAN_TICKS = 10;

// Live candidates one debt payment examines, and entries one write-driven idle scan does: enough
// to step over a few fresh groups, few enough that a write never walks a long queue.
const EVICT_SCAN = 4;

// Dead entries the eviction order tolerates beyond twice the live ones before it is rebuilt.
const EVICT_SLACK = 64;

/** The initial policy for a {@link Pool}. */
export interface Config {
	/** The byte target usage converges toward; unbounded when omitted. */
	capacity?: number;
	/** How long a group nobody reads or writes stays cached; never reclaimed for idleness when omitted. */
	expiry?: Duration;
}

/**
 * A shared byte budget and idle window for the cached groups of every track charging into it.
 *
 * The capacity is a target usage converges toward, not a hard limit: carried debt, capped
 * payments, and the always protected live edge all let usage exceed it for a while.
 */
export class Pool {
	#capacity: number;
	// The idle window in ticks.
	readonly #expiry?: number;
	#used = 0;
	// The first instant the pool read, and the latest tick since it, never moving back.
	#epoch?: Instant;
	#tick = 0;
	// The sum and count of the evictable groups' access stamps, for their mean.
	#sum = 0;
	#count = 0;
	// The accounts holding a group that may expire, which the idle sweep visits.
	#tracks = new Set<Track<Entry>>();
	#sweep?: AbortController;

	constructor(config: Config = {}) {
		this.#capacity = config.capacity ?? Number.POSITIVE_INFINITY;
		if (config.expiry !== undefined) this.#expiry = Math.max(1, Math.ceil(config.expiry.divDuration(TICK)));
	}

	/** The byte target, or undefined when unbounded. */
	get capacity(): number | undefined {
		return Number.isFinite(this.#capacity) ? this.#capacity : undefined;
	}

	/** Bytes billed across every cached group. */
	get used(): number {
		return this.#used;
	}

	/** The idle window, rounded up to the pool's 100 ms tick, or undefined when idle groups stay. */
	get expiry(): Duration | undefined {
		return this.#expiry === undefined ? undefined : TICK.mul(this.#expiry);
	}

	/** Change the byte target; undefined makes it unbounded. Writes pay a shrink down as they go. */
	resize(capacity?: number): void {
		this.#capacity = capacity ?? Number.POSITIVE_INFINITY;
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

	/** The mean access stamp of the evictable groups, or undefined with none. */
	average(): number | undefined {
		return this.#count === 0 ? undefined : this.#sum / this.#count;
	}

	/**
	 * The eviction debt writing `written` bytes accrues, or undefined while the pool is within
	 * capacity: `written * used / capacity`, so paying it frees a little more than was written
	 * and the overshoot decays toward the capacity.
	 */
	accrue(written: number): number | undefined {
		if (this.#used <= this.#capacity) return undefined;
		return Math.floor((written * this.#used) / Math.max(1, this.#capacity));
	}

	/** Bill `n` more bytes. */
	add(n: number): void {
		this.#used += n;
	}

	/** Release `n` billed bytes. */
	sub(n: number): void {
		this.#used -= n;
	}

	/** The current tick as an access stamp, with `boost` breaking ties within it. */
	stamp(boost: number): number {
		return this.now() * BOOSTS + boost;
	}

	/** A group with access stamp `stamp` became evictable. */
	accessInsert(stamp: number): void {
		this.#sum += stamp;
		this.#count++;
	}

	/** A group with access stamp `stamp` stopped being evictable. */
	accessRemove(stamp: number): void {
		this.#sum -= stamp;
		this.#count--;
	}

	/** An evictable group's access stamp moved from `old` to `stamp`. */
	accessRefresh(old: number, stamp: number): void {
		this.#sum += stamp - old;
	}

	/** Visit `track` on each idle sweep, if the pool has an idle window, while it holds a group that may expire. */
	register(track: Track<Entry>): void {
		if (this.#expiry === undefined) return;
		this.#tracks.add(track);
		this.#arm();
	}

	/** Stop visiting `track`, which holds no group that may expire. */
	unregister(track: Track<Entry>): void {
		this.#tracks.delete(track);
	}

	/** Expire the idle groups of every registered track. */
	sweep(): void {
		for (const track of [...this.#tracks]) track.sweep();
	}

	// Sweep again in half the idle window while any track holds a group that may expire. Never a
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

/** A cached group as its track's account orders it for eviction. */
export interface Entry {
	/** What the group bills. */
	readonly charge: Charge;
}

// A group's place in an account's eviction order, emptied once the group leaves the cache.
interface Slot<E> {
	entry: E | undefined;
}

/** How an account reaches the track whose groups it evicts. */
export interface Owner<E extends Entry> {
	/** Whether `entry` is the live edge, which neither eviction nor expiry takes. */
	protects(entry: E): boolean;
	/** The live edge while protected, whose front pays the debt when no group can go whole. */
	edge(): E | undefined;
	/** Take `entry` out of the cache and end it, for its bytes (`evicted`) or for idling (`expired`). */
	evict(entry: E, reason: "evicted" | "expired"): void;
	/** Shed whole frames from the front of `entry` toward `debt`, freeing about `cap` at most, and return the bytes freed. */
	shed(entry: E, debt: number, cap: number): number;
	/** Something was evicted: settle what the cache's contents decide. */
	settle(): void;
	/** Forget the lowest runs of the track's drop set toward `debt`, freeing about `cap` at most, and return the bytes freed. */
	shedDrops(debt: number, cap: number): number;
	/** Fold the oldest gaps of the track's arrivals toward `debt`, freeing about `cap` at most, and return the bytes freed. */
	shedArrivals(debt: number, cap: number): number;
	/** Forget the runs of the drop set given up longer than the pool's idle window before tick `now`, returning whether any went. */
	expireDrops(now: number): boolean;
	/** Whether the track holds a drop run a reclaim may forget, which keeps its account in the idle sweep. */
	holdsDrops(): boolean;
}

/**
 * One track's account against a {@link Pool}: the bytes its groups wrote, the debt they owe, and
 * the order its groups are evicted in. Mirrors the Rust `cache::Track` with the eviction order
 * and debt of the Rust `TrackState`.
 */
export class Track<E extends Entry> {
	/** The pool this track caches into. */
	readonly pool: Pool;
	#owner: Owner<E>;
	// Bytes billed since the last debt accrual.
	#written = 0;
	// The tick a frame write may next run an idle scan at.
	#nextExpiry = 0;
	// Every cached group with its slot in the eviction order, oldest first. A group leaving the
	// cache empties its slot, so the order never pins its frames; a scan drops empty slots. The
	// order starts at `#head`, since `Array.shift` moves every slot behind it in V8.
	#live = new Map<E, Slot<E>>();
	#order: Slot<E>[] = [];
	#head = 0;
	// Debt carried over from earlier payments.
	#debt = 0;
	// Where the next bounded idle scan starts, rotating so fresh groups at the front do not
	// starve the ones behind them.
	#cursor = 0;

	constructor(pool: Pool, owner: Owner<E>) {
		this.pool = pool;
		this.#owner = owner;
	}

	/** Open the charge for a new group, billing its fixed overhead. */
	charge(): Charge {
		return makeCharge(this as Track<Entry>);
	}

	/** Count `n` billed bytes toward the next debt accrual. */
	wrote(n: number): void {
		this.#written += n;
	}

	/** Bill `n` bytes the track keeps beside its groups, such as its drop set, which the idle sweep then visits. */
	bill(n: number): void {
		this.pool.add(n);
		this.wrote(n);
		if (this.#owner.holdsDrops()) this.pool.register(this as Track<Entry>);
	}

	/** Release `n` bytes {@link bill} billed. */
	unbill(n: number): void {
		this.pool.sub(n);
	}

	/** Whether the account caches any group. */
	get empty(): boolean {
		return this.#live.size === 0;
	}

	/** Cache `entry` at the back of the eviction order, evictable now unless it is the live edge. */
	insert(entry: E): void {
		const slot = { entry };
		this.#live.set(entry, slot);
		this.#order.push(slot);
		if (!this.#owner.protects(entry)) this.demote(entry);
	}

	/** Make `entry` evictable once the live edge no longer protects it. */
	demote(entry: E): void {
		if (!this.#live.has(entry)) return;
		entry.charge.demote();
		this.pool.register(this as Track<Entry>);
	}

	/** Forget `entry`, which left the cache, releasing what it billed. */
	remove(entry: E): void {
		const slot = this.#live.get(entry);
		if (slot === undefined) return;
		this.#live.delete(entry);
		slot.entry = undefined;
		entry.charge.clear();
		if (this.#live.size === 0) {
			this.#order = [];
			this.#head = 0;
			if (!this.#owner.holdsDrops()) {
				this.#debt = 0;
				this.pool.unregister(this as Track<Entry>);
			}
		}
	}

	/**
	 * Accrue and pay the eviction debt of everything written since the last accrual. Run before a
	 * new group is cached, so the write that creates it can never evict it. Doubles when the
	 * track's oldest group is staler than the pool's mean. Returns whether it evicted a group.
	 */
	chargeDebt(): boolean {
		const written = this.#written;
		this.#written = 0;
		const accrued = this.pool.accrue(written);
		// Under capacity, a stale debt would only make a burst at the next pressure spike.
		if (accrued === undefined) {
			this.#debt = 0;
			return false;
		}
		const debt = this.#oldestIsStale() ? 2 * accrued : accrued;
		// What is used bounds what eviction could ever free, so a track that cannot pay
		// (everything protected) does not hoard a stale schedule.
		this.#debt = Math.min(this.#debt + debt, this.pool.used);
		// One write never dumps a deep backlog at once: the remainder carries over.
		return this.#payDebt(2 * written);
	}

	// Whether the oldest evictable group was accessed no later than the pool's mean. A dead entry
	// at the front reads as not stale until a scan drops it.
	#oldestIsStale(): boolean {
		const average = this.pool.average();
		const front = this.#order[this.#head]?.entry;
		if (average === undefined || front === undefined) return false;
		return !this.#owner.protects(front) && front.charge.accessed <= average;
	}

	// Evict the stalest groups until the debt is paid or `cap` bytes are freed, examining a few
	// candidates from the front of the order. A group accessed after the pool's mean rotates to
	// the back instead, leaving the debt to tracks holding staler content. A group larger than
	// the debt left stays, carrying the debt over. With no group able to go whole, the fresh
	// ones and the live edge shed their fronts, so neither grows without bound.
	#payDebt(cap: number): boolean {
		const average = this.pool.average() ?? 0;
		let paid = 0;
		let scanned = 0;
		let evicted = false;
		const fresh: E[] = [];
		for (let i = this.#order.length - this.#head; i > 0; i--) {
			if (this.#debt === 0 || paid >= cap || scanned >= EVICT_SCAN) break;
			// An eviction that emptied the account reset the order.
			if (this.#head >= this.#order.length) break;
			const slot = this.#order[this.#head++];
			const entry = slot.entry;
			if (entry === undefined) continue;
			if (this.#owner.protects(entry)) {
				this.#order.push(slot);
				continue;
			}
			scanned++;
			if (entry.charge.accessed > average) {
				this.#order.push(slot);
				fresh.push(entry);
				continue;
			}
			const size = entry.charge.size;
			if (size > this.#debt) {
				// Back to the front, where its slot still is.
				this.#head--;
				this.#compact();
				return evicted;
			}
			this.#debt -= size;
			paid += size;
			evicted = true;
			this.#owner.evict(entry, "evicted");
		}
		// Then the arrivals' oldest gaps, which costs a reader only the wait for a late group, as
		// the grace passing would.
		if (this.#debt > 0 && paid < cap) {
			const freed = this.#owner.shedArrivals(this.#debt, cap - paid);
			this.#debt -= Math.min(freed, this.#debt);
			paid += freed;
		}
		// Then the drop set, oldest sequences first: losing a run costs a reader a judgment or a
		// fill, never content being read.
		if (this.#debt > 0 && paid < cap) {
			const freed = this.#owner.shedDrops(this.#debt, cap - paid);
			this.#debt -= Math.min(freed, this.#debt);
			paid += freed;
		}
		const edge = this.#owner.edge();
		if (edge) fresh.push(edge);
		for (const entry of fresh) {
			if (this.#debt === 0 || paid >= cap) break;
			if (!this.#live.has(entry)) continue;
			const freed = this.#owner.shed(entry, this.#debt, cap - paid);
			this.#debt -= Math.min(freed, this.#debt);
			paid += freed;
		}
		this.#compact();
		return evicted;
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
		const width = full ? len : Math.min(len, EVICT_SCAN);
		if (!full) this.#cursor = start + width;
		const expired: E[] = [];
		for (let step = 0; step < width; step++) {
			const entry = order[head + ((start + step) % len)].entry;
			if (entry === undefined || this.#owner.protects(entry)) continue;
			if (now - Math.floor(entry.charge.accessed / BOOSTS) > expiry) expired.push(entry);
		}
		for (const entry of expired) this.#owner.evict(entry, "expired");
		this.#compact();
		return expired.length > 0 || forgot;
	}

	// Skip the empty slots at the front of the order, and rebuild it once the slots before the
	// front and the empty ones behind it clearly outnumber the live ones.
	#compact(): void {
		const order = this.#order;
		while (this.#head < order.length && order[this.#head].entry === undefined) this.#head++;
		if (order.length > 2 * this.#live.size + EVICT_SLACK) {
			this.#order = order.slice(this.#head).filter((slot) => slot.entry !== undefined);
			this.#head = 0;
		}
	}

	/**
	 * Settle from a frame write: pay the debt once enough was written, and scan for idle groups
	 * once per second, so a track that only appends frames to open groups still pays and ages.
	 */
	settle(): void {
		const debt = this.#written >= WRITE_CHARGE_THRESHOLD;
		const expiry = this.#expiryDue();
		let evicted = false;
		if (debt) evicted = this.chargeDebt();
		if (expiry) evicted = this.evictExpired(false) || evicted;
		if (evicted) this.#owner.settle();
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
 * What one cached group bills to its track's account: its bytes, overhead included, and its
 * access stamp, which the pool's mean samples while the group is evictable. Cleared once the
 * group leaves the cache, after which every operation is a no-op.
 */
export class Charge {
	#track?: Track<Entry>;
	#bytes = 0;
	#stamp: number;
	// Whether the stamp is a sample in the pool's mean: the group is evictable.
	#counted = false;

	private constructor(track: Track<Entry>) {
		this.#track = track;
		this.#stamp = track.pool.stamp(0);
		this.bill(ENTRY_OVERHEAD);
	}

	static {
		makeCharge = (track) => new Charge(track);
	}

	/** Bill `n` more bytes of a write, which is an access too: it restarts the idle window. */
	add(n: number): void {
		this.bill(n);
		this.#touch(WRITE_BOOST);
	}

	/** Bill `n` more bytes nobody accessed, such as a subscriber's mirror. */
	bill(n: number): void {
		const track = this.#track;
		if (!track) return;
		track.pool.add(n);
		track.wrote(n);
		this.#bytes += n;
	}

	/** Release `n` bytes the group dropped while still cached. */
	sub(n: number): void {
		const track = this.#track;
		if (!track) return;
		n = Math.min(n, this.#bytes);
		track.pool.sub(n);
		this.#bytes -= n;
	}

	/** Every byte billed, overhead included. */
	get size(): number {
		return this.#bytes;
	}

	/** The last access stamp: its tick times four, plus the boost of the access that set it. */
	get accessed(): number {
		return this.#stamp;
	}

	/** Record a read: a fetched group or a frame read. */
	refresh(): void {
		this.#touch(READ_BOOST);
	}

	/** Record a write that bills no new bytes. */
	recordWrite(): void {
		this.#touch(WRITE_BOOST);
	}

	/** Make the group evictable, sampling its stamp into the pool's mean. Idempotent. */
	demote(): void {
		const track = this.#track;
		if (!track || this.#counted) return;
		track.pool.accessInsert(this.#stamp);
		this.#counted = true;
	}

	/** Settle the account's debt and idle scan after a frame write. */
	settle(): void {
		this.#track?.settle();
	}

	/** Release everything billed and the mean's sample, and detach from the account. Idempotent. */
	clear(): void {
		const track = this.#track;
		if (!track) return;
		track.pool.sub(this.#bytes);
		this.#bytes = 0;
		if (this.#counted) track.pool.accessRemove(this.#stamp);
		this.#counted = false;
		this.#track = undefined;
	}

	// Advance the stamp to the current tick with `boost`, never moving it back.
	#touch(boost: number): void {
		const track = this.#track;
		if (!track) return;
		const stamp = track.pool.stamp(boost);
		if (stamp <= this.#stamp) return;
		if (this.#counted) track.pool.accessRefresh(this.#stamp, stamp);
		this.#stamp = stamp;
	}
}
