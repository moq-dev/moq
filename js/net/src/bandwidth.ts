/**
 * Rate estimation split among the tracks sharing one connection.
 *
 * One estimate covers a whole connection, so senders sharing it divide it with
 * an {@link Allocator} rather than each targeting the whole thing. How a sender
 * then follows its share is policy and lives with the sender.
 *
 * The same reservations set the connection's probe target: their ceilings summed,
 * which the connection hands to its padding sender so the estimate can rise to
 * fund them.
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
 * `undefined` when `id` isn't among the wants, which is how a released or closed
 * reservation reports "hold your rate" instead of a grant of zero.
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

interface Entry {
	id: number;
	demand: Demand;
	priority: number;
	max: number;
	rate: Signal<number | undefined>;
	probed: Signal<boolean>;
}

interface Registry {
	peek(id: number): number | undefined;
	update(id: number, max: number): void;
	release(id: number): void;
}

function wantsOf(entries: readonly Entry[]): Want[] {
	const wants: Want[] = [];
	for (const entry of entries) {
		if (hooks.demandReleased(entry.demand).peek() !== undefined) continue;
		if (entry.demand.wanted.peek() === undefined) continue;
		wants.push({ id: entry.id, priority: entry.priority, max: entry.max });
	}
	return wants;
}

/**
 * `entry`'s slice of `estimate` among the demanded `wants`.
 *
 * An idle entry claims nothing from the others but still reads the slice it would
 * get back: a rendition stopped for lack of bandwidth, whose viewers then left,
 * needs that to know when to resume.
 */
function shareOf(estimate: number, wants: readonly Want[], entry: Entry): number | undefined {
	if (entry.demand.closed.peek() !== undefined) return undefined;
	if (entry.demand.wanted.peek() === undefined) {
		wants = [...wants, { id: entry.id, priority: entry.priority, max: entry.max }];
	}
	return allocate(estimate, wants, entry.id);
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
	/** Override the probe target, for every sender sharing it; see {@link Allocator.setTarget}. */
	setTarget(target: number | undefined): void;
}

/**
 * An allocator's probe target, followed by the session that probes for it.
 *
 * @internal
 */
export interface Target {
	/** The rate to probe up to in bits per second; zero means don't probe. */
	readonly rate: Getter<number>;
	/** Stop probing for the reservations, so they no longer read as probed. Idempotent. */
	close(): void;
}

/**
 * Follow `allocator`'s probe target, for the session that probes on its reservations' behalf.
 *
 * Holding it is what marks the reservations {@link Reservation.probed}, so only a session
 * with a sender takes one.
 *
 * @internal
 */
export let follow: (allocator: Allocator) => Target;

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
 * The reservations also set the connection's probe target: the sum of every live
 * reservation's ceiling, demanded or not, so a sender that stopped for lack of
 * bandwidth keeps asking the connection for its room. A connection probes for its
 * own allocator, so reserve on `Connection.allocator` rather than building one: an
 * allocator from the constructor divides an estimate but probes for nothing.
 *
 * Hand the same instance to every sender on the connection.
 */
export class Allocator implements Handle {
	#estimate: Getter<number | undefined> | undefined;
	#entries = new Signal<Entry[]>([]);
	#alive = new Signal(true);
	// The application's override of the reservations' sum as the probe target.
	#override = new Signal<number | undefined>(undefined);
	#target = new Signal(0);
	// How many sessions probe for these reservations, each through a followed target.
	#probes = new Signal(0);
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

	static {
		follow = (allocator) => allocator.#follow();
	}

	/**
	 * Probe up to `target` instead of the sum of the reservations, or `undefined` to follow
	 * them again.
	 *
	 * Zero turns probing off. The override is the allocator's, so it holds across the
	 * reconnects a connection's allocator spans.
	 */
	setTarget(target: number | undefined): void {
		this.#override.set(target === undefined ? undefined : ceiling(target));
		this.#publish();
	}

	// Count a session probing for the reservations until the returned target closes.
	#follow(): Target {
		this.#probes.update((probes) => probes + 1);
		this.#publish();
		let closed = false;
		return {
			rate: this.#target,
			close: () => {
				if (closed) return;
				closed = true;
				this.#probes.update((probes) => probes - 1);
				this.#publish();
			},
		};
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
		const probed = new Signal(false);
		this.#entries.mutate((entries) => {
			entries.push({
				id,
				demand: track,
				priority: hooks.demandPriority(track),
				max,
				rate,
				probed,
			});
		});

		const reservation = makeReservation(this.#registry, id, rate, probed);
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
			entry.probed.set(false);
		}
		this.#entries.set([]);
		this.#target.set(0);
		this.#signals.close();
	}

	#prune(): void {
		const entries = this.#entries.peek();
		const live = entries.filter((entry) => hooks.demandReleased(entry.demand).peek() === undefined);
		if (live.length !== entries.length) this.#entries.set(live);
	}

	#publish(): void {
		if (!this.#alive.peek()) return;
		const estimate = this.#estimate?.peek();
		const entries = this.#entries.peek();
		const wants = wantsOf(entries);

		// Demanded or not: a rendition stopped for lack of bandwidth lost its viewers, and
		// still has to be funded to resume. A closed track claims nothing.
		let reserved = 0;
		for (const entry of entries) {
			if (hooks.demandReleased(entry.demand).peek() === undefined) reserved += entry.max;
		}
		const target = this.#override.peek() ?? reserved;
		this.#target.set(target);
		const probed = this.#probes.peek() > 0 && target > 0;

		for (const entry of entries) {
			entry.rate.set(estimate == null ? undefined : shareOf(estimate, wants, entry));
			entry.probed.set(probed);
		}
	}

	#peek(id: number): number | undefined {
		if (!this.#alive.peek()) return undefined;
		const estimate = this.#estimate?.peek();
		if (estimate == null) return undefined;
		const entries = this.#entries.peek();
		const entry = entries.find((candidate) => candidate.id === id);
		if (!entry) return undefined;
		return shareOf(estimate, wantsOf(entries), entry);
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

let makeReservation: (
	registry: Registry,
	id: number,
	rate: Signal<number | undefined>,
	probed: Signal<boolean>,
) => Reservation;

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
	readonly #probed: Signal<boolean>;
	#closed = false;

	private constructor(registry: Registry, id: number, rate: Signal<number | undefined>, probed: Signal<boolean>) {
		this.#registry = registry;
		this.#id = id;
		this.#rate = rate;
		this.#probed = probed;
	}

	static {
		makeReservation = (registry, id, rate, probed) => new Reservation(registry, id, rate, probed);
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
	 * Whether the connection probes for this reservation, so the estimate can rise to fund
	 * it even while its sender is stopped.
	 *
	 * Only a connection with a padding sender probes, and none does while
	 * {@link Allocator.setTarget} turns probing off. Without one the estimate stops learning
	 * once the connection is app-limited, so a sender that stops for lack of bandwidth could
	 * never see enough to resume.
	 */
	get probed(): Getter<boolean> {
		return this.#probed;
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
		this.#probed.set(false);
		this.#registry.release(this.#id);
	}

	/** Calls {@link close}, so `using` releases the claim. */
	[Symbol.dispose](): void {
		this.close();
	}
}
