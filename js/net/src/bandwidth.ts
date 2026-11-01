/**
 * Rate estimation split among the tracks sharing one connection.
 *
 * One estimate covers a whole connection, so senders sharing it divide it with
 * an {@link Allocator} rather than each targeting the whole thing. How a sender
 * then follows its share is policy and lives with the sender.
 *
 * @module
 */
import { Effect, type Getter, Signal } from "@moq/signals";
import { hooks } from "./internal.ts";
import type { Demand } from "./track.ts";

/**
 * One demanded track's claim, snapshotted for {@link allocate}.
 *
 * `id` is the allocator's, not the track's sequence: two reservations of the
 * same track are still two claims.
 */
export interface Want {
	/** Allocator-assigned identity of this claim. */
	id: number;
	/** Publisher priority; higher is served first. */
	priority: number;
	/** Ceiling in bits per second, not a measurement of current output. */
	max: number;
}

/** A want in the order it is served, with what was left of the estimate before it. */
interface Served {
	id: number;
	priority: number;
	max: number;
	budget: number;
}

/**
 * One division of `estimate` among the demanded `wants`, computed once and read by every
 * reservation instead of each re-dividing the whole table.
 *
 * Strict priority: a tier is filled to its reservations before the next one
 * sees a bit. Within a tier the split is max-min fair, so a share asking for
 * less than an even split takes all of it and leaves the rest to the others.
 *
 * Surplus above the total reserved is left unclaimed rather than spread around.
 * A reservation is what a sender can use, so handing it more is not a reason to
 * send more than it was configured for.
 *
 * Rates are bits per second.
 */
export class Plan {
	/**
	 * The wants in the order they are served: higher priority first, then ascending by
	 * reservation, then by registration.
	 */
	readonly #order: Served[];
	readonly #grants = new Map<number, number>();
	/** What is left once every want is served. */
	readonly #leftover: number;

	constructor(estimate: number, wants: readonly Want[]) {
		// Read each want once. Registration order breaks ties, as in the Rust allocator.
		const order = wants.map((want) => ({ id: want.id, priority: want.priority, max: want.max, budget: 0 }));
		order.sort((a, b) => b.priority - a.priority || a.max - b.max || a.id - b.id);

		// Integer bits per second: the same truncation the Rust allocator uses.
		let budget = estimate;
		for (let start = 0; start < order.length; ) {
			let end = start;
			while (end < order.length && order[end].priority === order[start].priority) end++;
			// Ascending by reservation: each share takes an even cut of what's left, or
			// all it asked for if that's less, which frees the difference for the rest.
			for (let at = start; at < end; at++) {
				const served = order[at];
				served.budget = budget;
				const grant = Math.min(served.max, Math.floor(budget / (end - at)));
				this.#grants.set(served.id, grant);
				budget -= grant;
			}
			start = end;
		}
		this.#order = order;
		this.#leftover = budget;
	}

	/**
	 * The slice for `id`, or `undefined` when `id` isn't among the wants, which is how a
	 * released or closed reservation reports "hold your rate" instead of a grant of zero.
	 */
	grant(id: number): number | undefined {
		return this.#grants.get(id);
	}

	/**
	 * What a want missing from this division would get were it demanded again.
	 *
	 * Tiers above its own are served first either way, so only its own tier is walked
	 * again, with one more want sharing it, as far as where it would be served: after
	 * every want of its tier reserving less than it, or as much but registered earlier.
	 */
	rejoin(want: Want): number {
		const { id, priority, max } = want;
		const order = this.#order;
		let start = 0;
		while (start < order.length && order[start].priority > priority) start++;
		let at = start;
		while (
			at < order.length &&
			order[at].priority === priority &&
			(order[at].max < max || (order[at].max === max && order[at].id < id))
		)
			at++;
		let end = at;
		while (end < order.length && order[end].priority === priority) end++;

		let budget = start < order.length ? order[start].budget : this.#leftover;
		let remaining = end - start + 1;
		for (let index = start; index < at; index++) {
			budget -= Math.min(order[index].max, Math.floor(budget / remaining));
			remaining -= 1;
		}
		return Math.min(max, Math.floor(budget / remaining));
	}
}

/**
 * Divide `estimate` among `wants`, returning the slice for `id`.
 *
 * `undefined` when `id` isn't among the wants. See {@link Plan}, which computes every
 * slice at once.
 */
export function allocate(estimate: number, wants: readonly Want[], id: number): number | undefined {
	return new Plan(estimate, wants).grant(id);
}

interface Entry {
	id: number;
	demand: Demand;
	priority: number;
	max: number;
	rate: Signal<number | undefined>;
}

interface Registry {
	peek(id: number): number | undefined;
	update(id: number, max: number): void;
	release(id: number): void;
}

/** Whether `entry` claims its share from the others: its track is demanded and not released. */
function demanded(entry: Entry): boolean {
	return hooks.demandReleased(entry.demand).peek() === undefined && entry.demand.wanted.peek() !== undefined;
}

/** A division and what it divided, so a read that finds them unchanged reuses it. */
interface Division {
	plan: Plan;
	estimate: number;
	/** The allocator's entry version it was divided at. */
	version: number;
	/** Which entries were {@link demanded}, in order. */
	demanded: boolean[];
}

/**
 * `entry`'s slice of `plan`, the division among the demanded entries.
 *
 * An idle entry claims nothing from the others but still reads the slice it would
 * get back: a rendition stopped for lack of bandwidth, whose viewers then left,
 * needs that to know when to resume.
 */
function shareOf(plan: Plan, entry: Entry): number | undefined {
	if (entry.demand.closed.peek() !== undefined) return undefined;
	if (entry.demand.wanted.peek() === undefined) return plan.rejoin(entry);
	return plan.grant(entry.id);
}

function ceiling(max: number): number {
	if (!Number.isFinite(max) || max < 0) {
		throw new Error(`reservation ceiling must be a finite non-negative number, got ${max}`);
	}
	return max;
}

/**
 * A non-owning handle on an allocator: reserve a slice, without its lifecycle.
 *
 * What a shared connection lends out. {@link Allocator} implements it, so code
 * that is handed an allocator rather than owning one should accept this type:
 * closing the registry stays the owner's alone, and a borrower cannot express it.
 *
 * @public
 */
export interface Handle {
	/** Reserve up to `max` for `track`; see {@link Allocator.reserve}. */
	reserve(track: Demand, max: number): Reservation;
}

/**
 * Divides one connection's bandwidth estimate among the tracks sharing it.
 *
 * Every sender on a connection reads the same estimate, so N senders each
 * targeting all of it oversubscribe the uplink N times over. Register a track
 * here and it gets a {@link Reservation} reporting only its own slice, so the
 * slices sum to the estimate instead of each matching it.
 *
 * Advisory, not enforced. A track that ignores its slice, or can't follow it at
 * all (PCM audio has a fixed bitrate), still sends what it sends; the transport
 * sheds the excess by dropping groups.
 *
 * Hand the same instance to every sender on the connection.
 */
export class Allocator implements Handle {
	#estimate: Getter<number | undefined> | undefined;
	#entries = new Signal<Entry[]>([]);
	#alive = new Signal(true);
	#nextId = 0;
	#signals = new Effect();
	#registry: Registry;
	/** Bumped whenever an entry is added, moved or removed. */
	#version = 0;
	#division: Division | undefined;

	/**
	 * Divide `estimate`, normally a connection's sampled send rate.
	 *
	 * Omit it (or call {@link unlimited}) for nothing to divide: every reservation
	 * reports `undefined`, which already means "no opinion, hold your rate".
	 */
	constructor(estimate?: Getter<number | undefined>) {
		this.#estimate = estimate;

		this.#registry = {
			peek: (id) => this.#peek(id),
			update: (id, max) => this.#update(id, max),
			release: (id) => this.#release(id),
		};

		this.#signals.run((effect) => {
			if (!effect.get(this.#alive)) return;
			if (this.#estimate) effect.get(this.#estimate);
			const entries = effect.get(this.#entries);
			for (const entry of entries) {
				effect.get(entry.demand.wanted);
				effect.get(hooks.demandReleased(entry.demand));
			}
			this.#publish();
		});
	}

	/**
	 * An allocator with nothing to divide, so every reservation reports `undefined`.
	 *
	 * `undefined` already means "no opinion, hold your rate" to a sender, so this is
	 * what a transport with no congestion estimate, a local file, or a test harness
	 * wants.
	 */
	static unlimited(): Allocator {
		return new Allocator();
	}

	/**
	 * Reserve up to `max` for `track`, returning the reservation.
	 *
	 * `max` is a ceiling, not a measurement: reserve the most the track can ever
	 * send, not what it happens to be sending. A VBR encoder sitting on a black
	 * screen at 1 Mbps can jump to 6 Mbps between one frame and the next, and a
	 * reservation that had followed it down would have already handed that room
	 * to somebody else.
	 *
	 * Priority comes from the track's `Info.priority` (higher served first). A tier is filled to
	 * its reservations before the next one sees a bit; within a tier the split is
	 * max-min fair.
	 *
	 * The reservation lasts until {@link Reservation.close}: hold it for as long
	 * as the sender is publishing, change the ceiling with
	 * {@link Reservation.update}, and close it to hand the room back.
	 *
	 * {@link Reservation.peek} / {@link Reservation.rate} report `undefined`
	 * while the connection has no estimate, telling a sender to hold its current
	 * rate rather than encode at zero. While nothing is subscribed to the track they
	 * report the slice the track would get back, without claiming it from the others.
	 */
	reserve(track: Demand, max: number): Reservation {
		max = ceiling(max);
		this.#prune();

		const id = this.#nextId++;
		const rate = new Signal<number | undefined>(undefined);
		this.#mutate((entries) => {
			entries.push({
				id,
				demand: track,
				priority: hooks.demandPriority(track),
				max,
				rate,
			});
		});

		const reservation = makeReservation(this.#registry, id, rate);
		this.#publish();
		return reservation;
	}

	/**
	 * Stop dividing. Existing reservations report `undefined` and further
	 * {@link reserve} calls still return a handle that never claims.
	 */
	close(): void {
		if (!this.#alive.peek()) return;
		this.#alive.set(false);
		for (const entry of this.#entries.peek()) {
			entry.rate.set(undefined);
		}
		this.#entries.set([]);
		this.#division = undefined;
		this.#signals.close();
	}

	#prune(): void {
		const entries = this.#entries.peek();
		const live = entries.filter((entry) => hooks.demandReleased(entry.demand).peek() === undefined);
		if (live.length !== entries.length) {
			this.#version++;
			this.#entries.set(live);
		}
	}

	#mutate(fn: (entries: Entry[]) => void): void {
		this.#version++;
		this.#entries.mutate(fn);
	}

	/** The division of `estimate` among `entries`, divided again only when either moved. */
	#plan(estimate: number, entries: readonly Entry[]): Plan {
		const last = this.#division;
		if (
			last?.estimate === estimate &&
			last.version === this.#version &&
			entries.every((entry, index) => demanded(entry) === last.demanded[index])
		) {
			return last.plan;
		}
		const flags = entries.map(demanded);
		const plan = new Plan(
			estimate,
			entries.filter((_, index) => flags[index]),
		);
		this.#division = { plan, estimate, version: this.#version, demanded: flags };
		return plan;
	}

	#publish(): void {
		if (!this.#alive.peek()) return;
		const estimate = this.#estimate?.peek();
		const entries = this.#entries.peek();
		// Divided once for every reservation rather than once per reservation.
		const plan = estimate == null ? undefined : this.#plan(estimate, entries);
		for (const entry of entries) {
			entry.rate.set(plan && shareOf(plan, entry));
		}
	}

	#peek(id: number): number | undefined {
		if (!this.#alive.peek()) return undefined;
		const estimate = this.#estimate?.peek();
		if (estimate == null) return undefined;
		const entries = this.#entries.peek();
		const entry = entries.find((candidate) => candidate.id === id);
		if (!entry) return undefined;
		return shareOf(this.#plan(estimate, entries), entry);
	}

	#update(id: number, max: number): void {
		if (!this.#alive.peek()) return;
		max = ceiling(max);
		this.#mutate((entries) => {
			const entry = entries.find((candidate) => candidate.id === id);
			if (entry) entry.max = max;
		});
		this.#publish();
	}

	#release(id: number): void {
		if (!this.#alive.peek()) return;
		this.#mutate((entries) => {
			const index = entries.findIndex((entry) => entry.id === id);
			if (index >= 0) entries.splice(index, 1);
		});
		this.#publish();
	}
}

let makeReservation: (registry: Registry, id: number, rate: Signal<number | undefined>) => Reservation;

/**
 * One track's standing claim on an {@link Allocator}, held for as long as the
 * sender that took it is publishing.
 *
 * Close it to release the claim; a forgotten reservation keeps claiming until
 * {@link close} (or {@link Symbol.dispose}) runs, and its siblings never get
 * the room.
 */
export class Reservation implements Disposable {
	readonly #registry: Registry;
	readonly #id: number;
	readonly #rate: Signal<number | undefined>;
	#closed = false;

	private constructor(registry: Registry, id: number, rate: Signal<number | undefined>) {
		this.#registry = registry;
		this.#id = id;
		this.#rate = rate;
	}

	static {
		makeReservation = (registry, id, rate) => new Reservation(registry, id, rate);
	}

	/**
	 * This reservation's slice right now.
	 *
	 * Stateless, unlike {@link rate}, which is a signal of what was last
	 * published and so can lag a synchronous {@link update} by a microtask if
	 * something else writes the registry first. Call this after {@link update}.
	 */
	peek(): number | undefined {
		if (this.#closed) return undefined;
		return this.#registry.peek(this.#id);
	}

	/**
	 * This reservation's current slice of the estimate.
	 *
	 * `undefined` while the connection has no estimate or this reservation has
	 * been closed: hold the current rate rather than encode at zero. While nothing
	 * is subscribed it is the slice the track would get back.
	 */
	get rate(): Getter<number | undefined> {
		return this.#rate;
	}

	/**
	 * Change the ceiling, keeping the same claim.
	 *
	 * For a sender whose ceiling genuinely moved: an encoder reopening at a
	 * resolution it negotiated with the device, not an encoder observing its own
	 * output.
	 */
	update(max: number): void {
		if (this.#closed) return;
		this.#registry.update(this.#id, max);
	}

	/** Release the claim so siblings take the room. Idempotent. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#rate.set(undefined);
		this.#registry.release(this.#id);
	}

	/** Calls {@link close}, so `using` releases the claim. */
	[Symbol.dispose](): void {
		this.close();
	}
}
