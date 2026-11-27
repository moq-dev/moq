/**
 * A broadcast's track listing: the tracks it lists, reported to each reader in order.
 *
 * A track is listed while it is in the broadcast's table: created by its producer (a track of
 * groups until it aborts or is removed, a datagram track until it ends), or answered by a
 * {@link Lister}, as a session lists its peer's tracks. A track served only on request is never
 * listed, and listing never requests one.
 *
 * Datagram tracks are a namespace apart from tracks of groups, so a name is listed at most once
 * per kind.
 *
 * Each {@link Tracks} reader keeps its own queue, keyed by kind and name in insertion order, so a
 * stalled reader holds at most one pending change per listed track: an add and a removal it has
 * not reached cancel, and a removal and re-add of a track it saw collapse in place. Its state is
 * bounded by the tracks it saw plus the live set, however long it stalls.
 *
 * @module
 */
import { type GetPromise, type Getter, Signal } from "@moq/signals";
import type * as datagram from "./datagram.ts";
import { StreamCode, StreamError } from "./error.ts";
import { hooks } from "./internal.ts";
import type * as track from "./track.ts";

/**
 * A listed track's kind and fixed properties: a track of groups with its {@link track.Info}, or
 * with `datagrams` set, a datagram track with its {@link datagram.Info}.
 *
 * @public
 */
export type Listed = { datagrams?: false; info: track.Info } | { datagrams: true; info: datagram.Info };

/**
 * What a {@link Tracks} listing reports, in order.
 *
 * `added`: a track the broadcast lists, with its kind and fixed properties. `synced`: every
 * track listed once the listing was complete (at once for a local broadcast, on its upstream's
 * SYNCED for a session's copy), and still listed, was reported; sent once, and later events are
 * live. `removed`: a track an earlier `added` reported is no longer listed,
 * unfinished: its producer aborted, or the answer that listed it withdrew it; a handler may still
 * serve its name on request. `finished`: a track an earlier `added` reported is no longer listed
 * because it finished. A removal of a datagram track sets `datagrams`, as its `added` did.
 *
 * @public
 */
export type TracksEvent =
	| ({ kind: "added"; name: string } & Listed)
	| { kind: "synced" }
	| { kind: "removed"; name: string; datagrams?: boolean }
	| { kind: "finished"; name: string; datagrams?: boolean };

/** A producer whose track the listing lists. */
type Own = { datagrams?: false; producer: track.Producer } | { datagrams: true; producer: datagram.Producer };

/**
 * Whether `own` leaves the listing as finished: it closed cleanly, or a track of groups declared
 * its end and is still open, as the Rust listing counts a producer that finished but still holds
 * its track.
 */
function finished(own: Own): boolean {
	if (own.datagrams) return own.producer.closed.peek() === null;
	const released = hooks.trackReleased(own.producer).peek();
	return released === null || (released === undefined && hooks.trackFinished(own.producer));
}

/**
 * The key a listed track is held under: its kind, then its name, so the kinds never collide.
 *
 * @internal
 */
export function keyOf(name: string, datagrams: boolean | undefined): string {
	return (datagrams ? "d" : "t") + name;
}

/** The event for the track under `key` leaving the listing. */
function removal(key: string, finished: boolean): TracksEvent {
	const kind = finished ? "finished" : "removed";
	const name = key.slice(1);
	return key[0] === "d" ? { kind, name, datagrams: true } : { kind, name };
}

/** One listed track and who lists it. */
interface Entry {
	listed: Listed;
	/** The producer whose track lists it, so a stale unlist never removes a later track of the name. */
	own?: Own;
	/** Whether the lister's answer lists it. */
	answered: boolean;
}

/**
 * A change a reader has yet to read. `readd` is a removal of a track it saw, then a track of the
 * same kind and name listed again. A removal carries whether the track finished.
 */
type Pending =
	| { kind: "add"; listed: Listed }
	| { kind: "remove"; finished: boolean }
	| { kind: "readd"; finished: boolean; listed: Listed };

/** One reader's pending changes, keyed by kind and name in the order they happened. */
class Queue {
	pending = new Map<string, Pending>();
	/** Events left to send before SYNCED, once the listing has synced. */
	bound?: number;
	/** SYNCED was sent. */
	synced = false;
	/** `null` once the listing ended, or the error that ended it. */
	end?: Error | null;
	/** The reader left the listing, or a newer reader of its owner replaced it. */
	detached = false;
	changed = new Signal(0);

	wake() {
		this.changed.update((n) => n + 1);
	}

	add(key: string, listed: Listed) {
		// A track is listed once at a time, so only its removal can be pending.
		const pending = this.pending.get(key);
		if (pending?.kind === "remove") this.pending.set(key, { kind: "readd", finished: pending.finished, listed });
		else this.pending.set(key, { kind: "add", listed });
		this.wake();
	}

	remove(key: string, finished: boolean) {
		// Never read: the two cancel. Otherwise the removal replaces a pending re-add in place.
		if (this.pending.get(key)?.kind === "add") this.pending.delete(key);
		else this.pending.set(key, { kind: "remove", finished });
		this.wake();
	}

	/** The events the pending changes take to read. */
	events(): number {
		let events = 0;
		for (const pending of this.pending.values()) events += pending.kind === "readd" ? 2 : 1;
		return events;
	}

	/** The listing synced: SYNCED goes out once what is queued now was read, or earlier if the queue drains first. */
	sync() {
		if (this.synced || this.bound !== undefined) return;
		this.bound = this.events();
		this.wake();
	}

	/** The listing's answer restarted before this reader was told it synced. */
	unsync() {
		if (!this.synced) this.bound = undefined;
	}

	finish(end: Error | null) {
		this.end ??= end;
		this.detached = true;
		this.wake();
	}

	ready(): boolean {
		return this.end !== undefined || this.pending.size > 0 || (!this.synced && this.bound !== undefined);
	}

	next(): TracksEvent | undefined {
		if (this.end) throw this.end;
		if (this.end === null) {
			// A clean end still reports each track that left before it, and how; an add the
			// reader never read is dropped.
			for (const [key, pending] of this.pending) {
				this.pending.delete(key);
				if (pending.kind !== "add") return removal(key, pending.finished);
			}
			return undefined;
		}
		if (!this.synced && this.bound !== undefined && (this.bound === 0 || this.pending.size === 0)) {
			this.synced = true;
			return { kind: "synced" };
		}
		const first = this.pending.entries().next();
		if (first.done) throw new Error("a ready queue has a pending change");
		const [key, pending] = first.value;
		if (!this.synced && this.bound !== undefined) this.bound -= 1;
		switch (pending.kind) {
			case "add":
				this.pending.delete(key);
				return { kind: "added", name: key.slice(1), ...pending.listed };
			case "remove":
				this.pending.delete(key);
				return removal(key, pending.finished);
			case "readd":
				// Setting an existing key keeps its place, so the add is read next.
				this.pending.set(key, { kind: "add", listed: pending.listed });
				return removal(key, pending.finished);
		}
	}
}

/**
 * The listing half of a broadcast's state.
 *
 * @internal
 */
export class Listing {
	/** Every listed track, in the order it was listed. */
	#live = new Map<string, Entry>();
	/** The queue of every reader of the current answer. */
	#readers = new Set<Queue>();
	/** Each owner's reader. */
	#owners = new Map<object, Queue>();
	/** Whether a {@link Lister} exists. While one does, readers wait for its answer to sync. */
	#lister = false;
	/** Whether the lister's answer has synced. */
	#answered = false;
	#wanted = new Signal(false);
	#closed: GetPromise<null>;

	constructor(closed: GetPromise<null>) {
		this.#closed = closed;
	}

	/** Whether anyone lists the broadcast. */
	get wanted(): Getter<boolean> {
		return this.#wanted;
	}

	/** Settles once the broadcast ended. */
	get closed(): GetPromise<null> {
		return this.#closed;
	}

	#synced(): boolean {
		return !this.#lister || this.#answered;
	}

	#counted() {
		this.#wanted.set(this.#readers.size > 0);
	}

	/** Re-evaluate whether the listing has synced, telling readers when it changed. */
	#resync(was: boolean) {
		const now = this.#synced();
		if (!was && now) for (const queue of this.#readers) queue.sync();
		if (was && !now) for (const queue of this.#readers) queue.unsync();
	}

	#insert(key: string, entry: Entry) {
		this.#live.set(key, entry);
		for (const queue of this.#readers) queue.add(key, entry.listed);
	}

	/** Unlist the track under `key`, telling readers whether it `finished`. */
	#remove(key: string, finished: boolean) {
		if (!this.#live.delete(key)) return;
		for (const queue of this.#readers) queue.remove(key, finished);
	}

	/**
	 * A reader with every listed track queued, ending `owner`'s older one with CANCELLED so an
	 * owner's listings cost one queue per broadcast however many requests reach it.
	 */
	read(owner?: object): Queue {
		if (this.#closed.peek() !== undefined) {
			throw new StreamError(StreamCode.Unroutable, { message: "broadcast is closed" });
		}
		const queue = new Queue();
		for (const [key, entry] of this.#live) queue.pending.set(key, { kind: "add", listed: entry.listed });
		if (this.#synced()) queue.bound = queue.pending.size;
		this.#readers.add(queue);
		if (owner) {
			const older = this.#owners.get(owner);
			this.#owners.set(owner, queue);
			if (older) {
				this.#readers.delete(older);
				older.finish(new StreamError(StreamCode.Cancel, { message: "replaced by a newer listing" }));
			}
		}
		this.#counted();
		return queue;
	}

	/** A reader left, so it reads nothing more. A replaced or ended one already did. */
	release(queue: Queue, owner?: object) {
		queue.pending.clear();
		if (queue.detached) return;
		queue.finish(null);
		this.#readers.delete(queue);
		if (owner && this.#owners.get(owner) === queue) this.#owners.delete(owner);
		this.#counted();
	}

	/** List the track of groups `producer` created. */
	listTrack(producer: track.Producer, info: track.Info) {
		this.#listOwn(producer.name, { producer }, { info });
	}

	/** List the datagram track `producer` created. */
	listDatagrams(producer: datagram.Producer, info: datagram.Info) {
		this.#listOwn(producer.name, { datagrams: true, producer }, { datagrams: true, info });
	}

	#listOwn(name: string, own: Own, listed: Listed) {
		const key = keyOf(name, listed.datagrams);
		const entry = this.#live.get(key);
		if (!entry) {
			this.#insert(key, { listed, own, answered: false });
		} else if (entry.own && !entry.answered) {
			// The earlier track of the name has not unlisted yet, but it left the table for this
			// one: readers see it go, as it ended, and the new one arrive.
			this.#remove(key, finished(entry.own));
			this.#insert(key, { listed, own, answered: false });
		} else {
			// Listed already by the answer, and it never stopped being listed: only the owner moves.
			entry.own = own;
		}
	}

	/** Unlist the track `producer` created, as it ended, unless a later one took its name. */
	unlistOwn(producer: track.Producer | datagram.Producer, datagrams = false) {
		const entry = this.#live.get(keyOf(producer.name, datagrams));
		if (!entry?.own || entry.own.producer !== producer) return;
		const own = entry.own;
		entry.own = undefined;
		if (!entry.answered) this.#remove(keyOf(producer.name, datagrams), finished(own));
	}

	/** Lister support; see {@link Lister}. */
	attach() {
		if (this.#lister) throw new Error("the broadcast already has a lister");
		const was = this.#synced();
		this.#lister = true;
		this.#resync(was);
	}

	detachLister() {
		const was = this.#synced();
		this.#lister = false;
		// Synced from here on, with only the producer's tracks.
		this.#unanswer();
		this.#resync(was);
	}

	answerAdd(name: string, listed: Listed) {
		const key = keyOf(name, listed.datagrams);
		const entry = this.#live.get(key);
		if (entry?.answered) throw new Error(`duplicate listed track: ${name}`);
		if (entry) entry.answered = true;
		// Copied, so an event forwarded as the answer lends it nothing else.
		else
			this.#insert(key, {
				listed: listed.datagrams ? { datagrams: true, info: listed.info } : { info: listed.info },
				answered: true,
			});
	}

	answerRemove(name: string, finished: boolean, datagrams = false) {
		this.#answerRemove(keyOf(name, datagrams), finished);
	}

	#answerRemove(key: string, finished: boolean) {
		const entry = this.#live.get(key);
		if (!entry?.answered) throw new Error(`track not listed: ${key.slice(1)}`);
		entry.answered = false;
		if (!entry.own) this.#remove(key, finished);
	}

	answerSynced() {
		const was = this.#synced();
		this.#answered = true;
		this.#resync(was);
	}

	/** Drop the answer: every track it listed, and its sync. */
	answerClear() {
		const was = this.#synced();
		this.#unanswer();
		this.#resync(was);
	}

	#unanswer() {
		for (const [key, entry] of [...this.#live]) {
			if (entry.answered) this.#answerRemove(key, false);
		}
		this.#answered = false;
	}

	/**
	 * The broadcast ended: so does every listing of it, first unlisting each of its producer's tracks
	 * that finished. A handler's answer of the same name does not keep it live, since the producer's
	 * track is what served it.
	 */
	close() {
		for (const [key, entry] of [...this.#live]) {
			if (entry.own && finished(entry.own)) this.#remove(key, true);
		}
		this.detach(null);
	}

	/** End every reader with `end` and detach them. */
	detach(end: Error | null) {
		for (const queue of this.#readers) queue.finish(end);
		this.#readers.clear();
		this.#owners.clear();
		this.#counted();
	}
}

let makeTracks: (listing: Listing, owner?: object) => Tracks;

/**
 * List a broadcast's tracks, ending `owner`'s older listing of it with CANCELLED.
 *
 * @internal
 */
export function listTracks(listing: Listing, owner?: object): Tracks {
	return makeTracks(listing, owner);
}

/**
 * The tracks a broadcast lists, from its consumer's `tracks()`.
 *
 * Reports every track listed once the listing is complete, then `synced`, then each change as it
 * happens, until the broadcast ends: a clean end comes after each track that left before it, and
 * a track still listed then ends unfinished. Listing a broadcast never requests a track: subscribe to
 * each listed one through the consumer's `track(name)`.
 *
 * @public
 */
export class Tracks {
	#listing: Listing;
	#queue: Queue;
	#owner?: object;

	private constructor(listing: Listing, owner?: object) {
		this.#listing = listing;
		this.#queue = listing.read(owner);
		this.#owner = owner;
	}

	static {
		makeTracks = (listing, owner) => new Tracks(listing, owner);
	}

	/**
	 * The next event, or undefined once the broadcast ended or this listing closed. Throws the
	 * error that ended the listing, such as the peer refusing it.
	 */
	async next(): Promise<TracksEvent | undefined> {
		for (;;) {
			if (this.#queue.ready()) return this.#queue.next();
			await Signal.race(this.#queue.changed);
		}
	}

	/** The events as they arrive; leaving the loop early closes the listing. */
	async *[Symbol.asyncIterator](): AsyncGenerator<TracksEvent, void, undefined> {
		try {
			for (;;) {
				const event = await this.next();
				if (!event) return;
				yield event;
			}
		} finally {
			this.close();
		}
	}

	/** Stop listing. Idempotent. */
	close(): void {
		this.#listing.release(this.#queue, this.#owner);
	}
}

/** Make the lister attached to a broadcast's listing state. @internal */
export let makeLister: (listing: Listing) => Lister;

/**
 * Answers a broadcast's listings for a handler that knows the tracks it would serve, as a
 * session lists its peer's. A broadcast has at most one.
 *
 * While it exists, readers wait for it to {@link synced} before reporting `synced`. Answer
 * while {@link wanted}: list what the publisher behind it holds and keep it current, then
 * {@link clear} once unwanted. {@link close} withdraws what it listed; a closed lister answers
 * nothing more.
 */
export class Lister {
	readonly #listing: Listing;
	// The listing this lister answers into, until it closes.
	#answering?: Listing;

	private constructor(listing: Listing) {
		listing.attach();
		this.#listing = listing;
		this.#answering = listing;
	}

	static {
		makeLister = (listing) => new Lister(listing);
	}

	/** Whether anyone lists the broadcast. */
	get wanted(): Getter<boolean> {
		return this.#listing.wanted;
	}

	/** Settles once the broadcast ended, as every listing of it does. */
	get closed(): GetPromise<null> {
		return this.#listing.closed;
	}

	/** List a track of the kind `listed` says. Throws if this answer already lists that kind and name. */
	add(name: string, listed: Listed): void {
		this.#answering?.answerAdd(name, listed);
	}

	/**
	 * Stop listing a track that left unfinished, reported as `removed`; a datagram track with
	 * `datagrams`. Throws if this answer does not list that kind and name.
	 */
	remove(name: string, datagrams = false): void {
		this.#answering?.answerRemove(name, false, datagrams);
	}

	/**
	 * Stop listing a track that finished, reported as `finished`; a datagram track with
	 * `datagrams`. Throws if this answer does not list that kind and name.
	 */
	finish(name: string, datagrams = false): void {
		this.#answering?.answerRemove(name, true, datagrams);
	}

	/** The answer holds everything this hop knows now; later changes are live. */
	synced(): void {
		this.#answering?.answerSynced();
	}

	/** Withdraw the answer: every track it listed, and its sync. */
	clear(): void {
		this.#answering?.answerClear();
	}

	/** End every current reader as if the broadcast ended, and withdraw the answer. A later reader asks again. */
	end(): void {
		const listing = this.#answering;
		listing?.detach(null);
		listing?.answerClear();
	}

	/** Fail every current reader with `err`, and withdraw the answer. A later reader asks again. */
	abort(err: Error): void {
		const listing = this.#answering;
		listing?.detach(err);
		listing?.answerClear();
	}

	/** Stop answering: the broadcast lists only its producer's tracks from here on. Idempotent. */
	close(): void {
		this.#answering?.detachLister();
		this.#answering = undefined;
	}
}
