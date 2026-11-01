/**
 * A broadcast's track listing: the tracks it lists, reported to each reader in order.
 *
 * A track is listed while it is in the broadcast's table: created by its producer until it
 * aborts or is removed, or answered by a {@link Lister}, as a session lists its peer's tracks.
 * A track served only on request is never listed, and listing never requests one.
 *
 * Each {@link Tracks} reader keeps its own queue, keyed by name in insertion order, so a
 * stalled reader holds at most one pending change per name: an add and a removal it has not
 * reached cancel, and a removal and re-add of a name it saw collapse in place. Its state is
 * bounded by the names it saw plus the live set, however long it stalls.
 *
 * @module
 */
import { type GetPromise, type Getter, Signal } from "@moq/signals";
import { StreamCode, StreamError } from "./error.ts";
import { hooks } from "./internal.ts";
import type * as track from "./track.ts";

/**
 * What a {@link Tracks} listing reports, in order.
 *
 * `added`: a track the broadcast lists, with its fixed properties. `synced`: every track
 * listed when the listing began, and still listed, was reported; sent once, and later events
 * are live. `removed`: a track an earlier `added` reported is no longer listed, unfinished: its
 * producer aborted, or the answer that listed it withdrew it; a handler may still serve its name
 * on request. `finished`: a track an earlier `added` reported is no longer listed because it
 * finished.
 *
 * @public
 */
export type TracksEvent =
	| { kind: "added"; name: string; info: track.Info }
	| { kind: "synced" }
	| { kind: "removed"; name: string }
	| { kind: "finished"; name: string };

/**
 * Whether `own` leaves the listing as finished: it closed cleanly, or it declared its end and is
 * still open, as the Rust listing counts a producer that finished but still holds its track.
 */
function finished(own: track.Producer): boolean {
	const released = hooks.trackReleased(own).peek();
	return released === null || (released === undefined && hooks.trackFinished(own));
}

/** One listed track and who lists it. */
interface Entry {
	info: track.Info;
	/** The producer whose track lists it, so a stale unlist never removes a later track of the name. */
	own?: track.Producer;
	/** Whether the lister's answer lists it. */
	answered: boolean;
}

/**
 * A change a reader has yet to read. `readd` is a removal of a name it saw, then the name listed
 * again. A removal carries whether the track finished.
 */
type Pending =
	| { kind: "add"; info: track.Info }
	| { kind: "remove"; finished: boolean }
	| { kind: "readd"; finished: boolean; info: track.Info };

/** One reader's pending changes, keyed by name in the order they happened. */
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

	add(name: string, info: track.Info) {
		// A name is listed once at a time, so only its removal can be pending.
		const pending = this.pending.get(name);
		if (pending?.kind === "remove") this.pending.set(name, { kind: "readd", finished: pending.finished, info });
		else this.pending.set(name, { kind: "add", info });
		this.wake();
	}

	remove(name: string, finished: boolean) {
		// Never read: the two cancel. Otherwise the removal replaces a pending re-add in place.
		if (this.pending.get(name)?.kind === "add") this.pending.delete(name);
		else this.pending.set(name, { kind: "remove", finished });
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
			for (const [name, pending] of this.pending) {
				this.pending.delete(name);
				if (pending.kind !== "add") return { kind: pending.finished ? "finished" : "removed", name };
			}
			return undefined;
		}
		if (!this.synced && this.bound !== undefined && (this.bound === 0 || this.pending.size === 0)) {
			this.synced = true;
			return { kind: "synced" };
		}
		const first = this.pending.entries().next();
		if (first.done) throw new Error("a ready queue has a pending change");
		const [name, pending] = first.value;
		if (!this.synced && this.bound !== undefined) this.bound -= 1;
		switch (pending.kind) {
			case "add":
				this.pending.delete(name);
				return { kind: "added", name, info: pending.info };
			case "remove":
				this.pending.delete(name);
				return { kind: pending.finished ? "finished" : "removed", name };
			case "readd":
				// Setting an existing key keeps its place, so the add is read next.
				this.pending.set(name, { kind: "add", info: pending.info });
				return { kind: pending.finished ? "finished" : "removed", name };
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

	#insert(name: string, entry: Entry) {
		this.#live.set(name, entry);
		for (const queue of this.#readers) queue.add(name, entry.info);
	}

	/** Unlist `name`, telling readers whether it `finished`. */
	#remove(name: string, finished: boolean) {
		if (!this.#live.delete(name)) return;
		for (const queue of this.#readers) queue.remove(name, finished);
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
		for (const [name, entry] of this.#live) queue.pending.set(name, { kind: "add", info: entry.info });
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

	/** List a track `own` created. */
	listOwn(name: string, own: track.Producer, info: track.Info) {
		const entry = this.#live.get(name);
		if (!entry) {
			this.#insert(name, { info, own, answered: false });
		} else if (entry.own && !entry.answered) {
			// The earlier track of the name has not unlisted yet, but it left the table for this
			// one: readers see it go, as it ended, and the new one arrive.
			this.#remove(name, finished(entry.own));
			this.#insert(name, { info, own, answered: false });
		} else {
			// Listed already by the answer, and it never stopped being listed: only the owner moves.
			entry.own = own;
		}
	}

	/** Unlist a track `own` created, as it ended, unless a later one took its name. */
	unlistOwn(name: string, own: track.Producer) {
		const entry = this.#live.get(name);
		if (entry?.own !== own) return;
		entry.own = undefined;
		if (!entry.answered) this.#remove(name, finished(own));
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

	answerAdd(name: string, info: track.Info) {
		const entry = this.#live.get(name);
		if (entry?.answered) throw new Error(`duplicate listed track: ${name}`);
		if (entry) entry.answered = true;
		else this.#insert(name, { info, answered: true });
	}

	answerRemove(name: string, finished: boolean) {
		const entry = this.#live.get(name);
		if (!entry?.answered) throw new Error(`track not listed: ${name}`);
		entry.answered = false;
		if (!entry.own) this.#remove(name, finished);
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
		for (const [name, entry] of [...this.#live]) {
			if (entry.answered) this.answerRemove(name, false);
		}
		this.#answered = false;
	}

	/**
	 * The broadcast ended: so does every listing of it, first unlisting each of its producer's tracks
	 * that finished. A handler's answer of the same name does not keep it live, since the producer's
	 * track is what served it.
	 */
	close() {
		for (const [name, entry] of [...this.#live]) {
			if (entry.own && finished(entry.own)) this.#remove(name, true);
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
 * Reports every track listed when it was created, then `synced`, then each change as it
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

/**
 * Answers a broadcast's listings for a handler that knows the tracks it would serve, as a
 * session lists its peer's. A broadcast has at most one.
 *
 * While it exists, readers wait for it to {@link synced} before reporting `synced`. Answer
 * while {@link wanted}: list what the publisher behind it holds and keep it current, then
 * {@link clear} once unwanted. {@link close} withdraws what it listed.
 */
export class Lister {
	#listing: Listing;
	#closed = false;

	/** @internal */
	constructor(listing: Listing) {
		listing.attach();
		this.#listing = listing;
	}

	/** Whether anyone lists the broadcast. */
	get wanted(): Getter<boolean> {
		return this.#listing.wanted;
	}

	/** Settles once the broadcast ended, as every listing of it does. */
	get closed(): GetPromise<null> {
		return this.#listing.closed;
	}

	/** List a track. Throws if this answer already lists it. */
	add(name: string, info: track.Info): void {
		this.#listing.answerAdd(name, info);
	}

	/** Stop listing a track that left unfinished, reported as `removed`. Throws if this answer does not list it. */
	remove(name: string): void {
		this.#listing.answerRemove(name, false);
	}

	/** Stop listing a track that finished, reported as `finished`. Throws if this answer does not list it. */
	finish(name: string): void {
		this.#listing.answerRemove(name, true);
	}

	/** The answer holds everything this hop knows now; later changes are live. */
	synced(): void {
		this.#listing.answerSynced();
	}

	/** Withdraw the answer: every track it listed, and its sync. */
	clear(): void {
		this.#listing.answerClear();
	}

	/** End every current reader as if the broadcast ended, and withdraw the answer. A later reader asks again. */
	end(): void {
		this.#listing.detach(null);
		this.#listing.answerClear();
	}

	/** Fail every current reader with `err`, and withdraw the answer. A later reader asks again. */
	abort(err: Error): void {
		this.#listing.detach(err);
		this.#listing.answerClear();
	}

	/** Stop answering: the broadcast lists only its producer's tracks from here on. Idempotent. */
	close(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#listing.detachLister();
	}
}
