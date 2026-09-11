/**
 * Rate estimation split among the tracks sharing one connection.
 *
 * One estimate covers a whole connection, so senders sharing it divide it with
 * an {@link Allocator} rather than each targeting the whole thing. How a sender
 * then follows its share is policy and lives with the sender.
 *
 * @module
 */
import { Effect, type GetPromise, type Getter, Signal } from "@moq/signals";

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

/**
 * Divide `estimate` among `wants`, returning the slice for `id`.
 *
 * Strict priority: a tier is filled to its reservations before the next one
 * sees a bit. Within a tier the split is max-min fair, so a share asking for
 * less than an even split takes all of it and leaves the rest to the others.
 *
 * Surplus above the total reserved is left unclaimed rather than spread around.
 * A reservation is what a sender can use, so handing it more is not a reason to
 * send more than it was configured for.
 *
 * `undefined` when `id` isn't among the wants, which is how an idle or closed
 * track reports "hold your rate" instead of a grant of zero.
 *
 * Rates are bits per second.
 */
export function allocate(estimate: number, wants: readonly Want[], id: number): number | undefined {
	// Integer bits per second: the same truncation the Rust allocator uses.
	let budget = estimate;
	let tier: number | undefined;
	for (const want of wants) {
		if (tier === undefined || want.priority > tier) tier = want.priority;
	}

	while (tier !== undefined) {
		// Ascending by reservation: each share takes an even cut of what's left, or
		// all it asked for if that's less, which frees the difference for the rest.
		const members = wants.filter((want) => want.priority === tier).sort((a, b) => a.max - b.max);

		let remaining = members.length;
		for (const want of members) {
			const even = Math.floor(budget / remaining);
			const grant = Math.min(want.max, even);
			if (want.id === id) return grant;
			budget -= grant;
			remaining -= 1;
		}

		let next: number | undefined;
		for (const want of wants) {
			if (want.priority < tier && (next === undefined || want.priority > next)) {
				next = want.priority;
			}
		}
		tier = next;
	}

	return undefined;
}

/** What {@link Allocator.reserve} reads off a track. */
export interface Demand {
	/** Whether any subscriber is currently attached. */
	readonly used: Getter<boolean>;
	/** Settles once the track closes. */
	readonly closed: GetPromise<Error | null>;
	/** Publisher priority; higher is served first. */
	readonly priority: number;
}

interface Entry {
	id: number;
	demand: Demand;
	priority: number;
	max: number;
	grant: Signal<number | undefined>;
}

interface Registry {
	peek(id: number): number | undefined;
	update(id: number, max: number): void;
	release(id: number): void;
}

function wantsOf(entries: readonly Entry[]): Want[] {
	const wants: Want[] = [];
	for (const entry of entries) {
		if (entry.demand.closed.peek() !== undefined) continue;
		if (!entry.demand.used.peek()) continue;
		wants.push({ id: entry.id, priority: entry.priority, max: entry.max });
	}
	return wants;
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
				effect.get(entry.demand.used);
				effect.get(entry.demand.closed);
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
	 * Priority comes from the track (higher served first). A tier is filled to
	 * its reservations before the next one sees a bit; within a tier the split is
	 * max-min fair.
	 *
	 * The reservation lasts until {@link Reservation.close}: hold it for as long
	 * as the sender is publishing, change the ceiling with
	 * {@link Reservation.update}, and close it to hand the room back.
	 *
	 * {@link Reservation.peek} / {@link Reservation.grant} report `undefined`
	 * while nothing is subscribed to the track or the connection has no estimate.
	 * That tells a sender to hold its current rate rather than encode at zero.
	 */
	reserve(track: Demand, max: number): Reservation {
		max = ceiling(max);
		this.#prune();

		const id = this.#nextId++;
		const grant = new Signal<number | undefined>(undefined);
		this.#entries.mutate((entries) => {
			entries.push({
				id,
				demand: track,
				priority: track.priority,
				max,
				grant,
			});
		});

		const reservation = makeReservation(this.#registry, id, grant);
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
			entry.grant.set(undefined);
		}
		this.#entries.set([]);
		this.#signals.close();
	}

	#prune(): void {
		const entries = this.#entries.peek();
		const live = entries.filter((entry) => entry.demand.closed.peek() === undefined);
		if (live.length !== entries.length) this.#entries.set(live);
	}

	#publish(): void {
		if (!this.#alive.peek()) return;
		const estimate = this.#estimate?.peek();
		const entries = this.#entries.peek();
		const wants = wantsOf(entries);
		for (const entry of entries) {
			entry.grant.set(estimate == null ? undefined : allocate(estimate, wants, entry.id));
		}
	}

	#peek(id: number): number | undefined {
		if (!this.#alive.peek()) return undefined;
		const estimate = this.#estimate?.peek();
		if (estimate == null) return undefined;
		const entries = this.#entries.peek();
		if (!entries.some((entry) => entry.id === id)) return undefined;
		return allocate(estimate, wantsOf(entries), id);
	}

	#update(id: number, max: number): void {
		if (!this.#alive.peek()) return;
		max = ceiling(max);
		this.#entries.mutate((entries) => {
			const entry = entries.find((candidate) => candidate.id === id);
			if (entry) entry.max = max;
		});
		this.#publish();
	}

	#release(id: number): void {
		if (!this.#alive.peek()) return;
		this.#entries.mutate((entries) => {
			const index = entries.findIndex((entry) => entry.id === id);
			if (index >= 0) entries.splice(index, 1);
		});
		this.#publish();
	}
}

let makeReservation: (registry: Registry, id: number, grant: Signal<number | undefined>) => Reservation;

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
	readonly #grant: Signal<number | undefined>;
	#closed = false;

	private constructor(registry: Registry, id: number, grant: Signal<number | undefined>) {
		this.#registry = registry;
		this.#id = id;
		this.#grant = grant;
	}

	static {
		makeReservation = (registry, id, grant) => new Reservation(registry, id, grant);
	}

	/**
	 * This reservation's slice right now.
	 *
	 * Stateless, unlike {@link grant}, which is a signal of what was last
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
	 * `undefined` while nothing is subscribed, the connection has no estimate, or
	 * this reservation has been closed: hold the current rate rather than encode
	 * at zero.
	 */
	get grant(): Getter<number | undefined> {
		return this.#grant;
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
		this.#grant.set(undefined);
		this.#registry.release(this.#id);
	}

	/** Calls {@link close}, so `using` releases the claim. */
	[Symbol.dispose](): void {
		this.close();
	}
}
