/**
 * Serve a front's logical track straight from its routes' copies. Mirrors `model::resume` in
 * `rs/moq-net`.
 *
 * A {@link Front} is one broadcast served through one route at a time: the origin re-sources it
 * when another route of the same epoch takes over, since routes with one epoch serve one
 * broadcast and so hold the same groups and frames. A reader of a logical track reads the
 * serving route's copy directly, through its own subscription: nothing is copied, and with one
 * route it is a passthrough.
 *
 * When the front switches, each reader subscribes to the new copy from the newest group it
 * handed out. A replaced copy keeps delivering until the new copy delivers its first group, then
 * is bounded just below it (or at the newest group handed out, if later), so a group only the old
 * route got after the switch is not lost. It is kept while it may still deliver a group under its
 * bound the reader lacks (unread, or still landing within the grace a passed group is waited for),
 * and while a group still read is read from it or may continue from it (it holds that group, or may
 * yet get it). Nothing under the bound is waited for past the reader's budget. Datagrams are
 * ephemeral, so no copy is kept for them: a reader that never asked for a group keeps none.
 *
 * A new copy is asked from where the reader got to, so it never has the groups below the reader's
 * local floor that the copies it replaced cached. Lowering that floor asks the newest route for
 * them through a backfill: another copy of that route, bounded to the groups the floor newly
 * covers, and kept like a replaced copy.
 *
 * A group carries on across the switch through its {@link Recover}: once its copy stalls while a
 * newer route serves, it continues from the serving route's copy of the same group at the same
 * frame, found in that copy's cache or fetched from it. Once its copy fails, any other copy still
 * holding the group continues it first, since a group's name fixes its content.
 *
 * A group is handed out once however many copies deliver it: the reader skips any sequence its
 * subscriber already handed out, the same record that keeps a direct reader from taking a
 * re-created group twice.
 *
 * A reader asked for dispositions reports what the serving copy gave up, other than a route
 * failure, once no copy it replaced may still deliver it, and counts it as handed out so no copy
 * serves it later.
 *
 * The logical track ends cleanly once its serving copy does and no group the reader lacks may
 * still land below that end on a copy it replaced. Its `closed` settles and the reader leaves the
 * route, keeping each copy only while it holds something left to read, as a direct reader keeps
 * its buffer. Its end is the first one any copy declared, which every later copy has to match, and
 * no copy may hold a group or datagram at or past it.
 *
 * @module
 */
import { Instant, Milli, now, sleepUntil } from "@moq/clock";
import { type Dispose, type Getter, race, Signal } from "@moq/signals";
import * as broadcast from "./broadcast.ts";
import { Delivered } from "./delivered.ts";
import { NotFound, SessionError, StreamCode, StreamError, sessionClosed } from "./error.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import {
	type DropRun,
	finalSequence,
	type GroupRecover,
	groupBounds,
	hooks,
	insertDrop,
	insertRun,
	MAX_DATAGRAMS,
	type Recv,
	type RecvDatagram,
	type Replacement,
	type ResumeShell,
	type ResumeSource,
	type Run,
	removeRun,
	runGaps,
} from "./internal.ts";
import { grace } from "./ledger.ts";
import type { Lister, Tracks, TracksEvent } from "./listing.ts";
import * as track from "./track.ts";
import { type Broadcast as BroadcastWire, registerWire, wireOf } from "./wire.ts";

// Stream codes that report the route failing to deliver rather than judging what it carried:
// another copy may still deliver what it failed. Mirrors the Rust `route_failed`.
const ROUTE_FAILURES: ReadonlySet<number> = new Set([
	StreamCode.Internal,
	StreamCode.Cancel,
	StreamCode.SessionClosed,
	StreamCode.GoingAway,
	StreamCode.ControlTimeout,
]);

/** A copy claiming the same content with different metadata: refused rather than reinterpreted. */
class Incompatible extends Error {
	constructor(name: string) {
		super(`track ${name} changed its properties across routes`);
		this.name = "Incompatible";
	}
}

/**
 * Whether `err` is the route failing to deliver rather than judging what it carried, so a new copy
 * may fix it: its session closed or failed underneath it, or a delivery code. Anything else would
 * repeat on every copy.
 *
 * @internal
 */
export function routeFailed(err: Error): boolean {
	if (err instanceof StreamError) return ROUTE_FAILURES.has(err.code);
	return err instanceof SessionError || sessionClosed(err);
}

// Every `track.Info` field, so one added later fails to compile until it is listed here.
const INFO_FIELDS = Object.keys({
	compress: true,
	timescale: true,
	maxAge: true,
	priority: true,
	refresh: true,
	ietf: true,
} satisfies Record<keyof track.Info, true>) as (keyof track.Info)[];

/** Two copies claiming the same content must agree on all of it, like Rust's `Info` equality. */
function compatible(a: track.Info, b: track.Info): boolean {
	const [x, y] = [track.infoDefaults(a), track.infoDefaults(b)];
	return INFO_FIELDS.every((field) => x[field] === y[field]);
}

/**
 * Ask `source` what track `name` is, abandoning the question once `retired` aborts. A caller's
 * `hold` keeps the track wanted until it or `retired` aborts; without one, only until the answer.
 */
function ask(source: Source, name: string, retired: AbortSignal, hold?: AbortSignal): Promise<track.Info> {
	if (hold) return source.wire.resolveTrackInfo(name, AbortSignal.any([hold, retired]));
	const query = new AbortController();
	const onRetire = () => query.abort(retired.reason);
	if (retired.aborted) onRetire();
	else retired.addEventListener("abort", onRetire, { once: true });
	return source.wire.resolveTrackInfo(name, query.signal).finally(() => {
		retired.removeEventListener("abort", onRetire);
		query.abort();
	});
}

/** One route's broadcast, held while the front serves it or a reader still reads a copy of it. */
class Source {
	readonly wire: BroadcastWire;
	/** The table's handle this was served from, so serving it again is a no-op. */
	readonly from: broadcast.Consumer;
	/** Whether a session delivered it, read from the handle served rather than the clone. */
	readonly fromSession: boolean;
	#consumer: broadcast.Consumer;
	#holds = 0;

	constructor(from: broadcast.Consumer) {
		this.from = from;
		this.fromSession = wireOf(from).fromSession === true;
		this.#consumer = from.clone();
		this.wire = wireOf(this.#consumer);
	}

	retain(): void {
		this.#holds++;
	}

	release(): void {
		if (this.#holds === 0) throw new Error("route released more often than retained");
		if (--this.#holds === 0) this.#consumer.close();
	}
}

/** What the front says about one logical track: the copy to read now, and how it ended. */
class Route {
	readonly name: string;
	/** Bumped whenever `source` changes, so a reader can tell a replacement apart. */
	generation = 0;
	/** The serving route's broadcast; undefined between routes or once the front let go. */
	source?: Source;
	/** The last broadcast served, kept between routes: what a replacement must match. */
	served?: Source;
	/** No front is left to replace the copy: readers follow it to its end. */
	concluded = false;
	/** The front refused the replacement: the track ends with this once the serving copy runs out. */
	refusal?: Error;
	/** Bumped on every change above, waking parked reads. */
	readonly changed = new Signal(0);
	readonly readers = new Set<Reader>();
	/** Called once the last reader leaves. */
	idle: () => void = () => {};

	constructor(name: string) {
		this.name = name;
	}

	#bump(): void {
		this.changed.update((n) => n + 1);
	}

	/** Serve the track from `source` from now on, subscribing every reader to it. */
	serve(source: Source): void {
		// A refusal ends the track: no later source continues it, compatible or not.
		if (this.concluded || this.refusal || this.source === source) return;
		this.generation++;
		this.source = source;
		this.served = source;
		this.#bump();
		for (const reader of [...this.readers]) reader.sync();
	}

	/** Between routes: readers wait for the next one, keeping what they have. */
	park(): void {
		if (!this.source) return;
		this.generation++;
		this.source = undefined;
		this.#bump();
	}

	/** End the track once the serving copy runs out, rather than replacing it. */
	refuse(err: Error): void {
		this.refusal ??= err;
		this.#bump();
	}

	/**
	 * No front will replace the serving copy. Same generation: a reader on the copy keeps it as
	 * the serving one, while a reader arriving later finds nothing to read.
	 */
	conclude(): void {
		if (this.concluded) return;
		this.concluded = true;
		this.source = undefined;
		this.#bump();
	}

	leave(reader: Reader): void {
		if (this.readers.delete(reader) && this.readers.size === 0) this.idle();
	}
}

/** A route's copy, as one reader reads it. */
interface Copy {
	generation: number;
	source: Source;
	sub: track.Subscriber;
	/** Where the subscription was asked to start, so demand updates never ask for less. */
	floor?: number;
	/** Replaced by a newer route: its datagrams are the newer copy's to deliver. */
	replaced: boolean;
	/**
	 * Bounded once the newer copy delivered its first group: nothing past this group is taken
	 * from it. `null` when that leaves nothing. Unset while it is the only copy still feeding
	 * groups the newer one may never have.
	 */
	until?: number | null;
	/** The group channel ran out: cleanly (`null`) or failed. */
	done?: Error | null;
	/** The datagram channel ran out. */
	datagramsDone?: Error | null;
	/**
	 * When a newer group superseded every group under the bound: the newer copy's first group, or
	 * the track's end. What the reader still lacks from it is waited for from here, for no longer
	 * than the reader's budget.
	 */
	since?: Instant;
	/** Groups still read that came from this copy, which keep a replaced copy subscribed. */
	lease: number;
}

/** Subscribe to a route's copy, or to a failed stand-in when that route can no longer serve. */
function subscribe(wire: BroadcastWire, name: string, subscription: track.Subscription): track.Subscriber {
	try {
		return wire.subscribe(name, subscription);
	} catch (err) {
		const failed = new track.Producer(name);
		failed.abort(err instanceof Error ? err : new Error(String(err)));
		return failed.subscribe(subscription);
	}
}

/** Take a copy's next group, in sequence or arrival order. */
function take(copy: Copy, ordered: boolean): Recv {
	return ordered ? hooks.tryNextGroup(copy.sub) : hooks.tryRecvGroup(copy.sub, false);
}

/** The first group past where `copy` was replaced: 0 for one replaced before any group. */
function copyEnd(copy: Pick<Copy, "until">): number | undefined {
	if (copy.until === undefined) return undefined;
	return copy.until === null ? 0 : copy.until + 1;
}

/** One reader's subscription to a logical track; see the module docs. */
class Reader implements ResumeSource {
	readonly route: Route;
	#shell: ResumeShell;
	/** The route generation last read. */
	#generation?: number;
	/**
	 * Oldest first, except that a backfill goes before the copy of the route it backfills: the last
	 * one is the serving route's own, when it is subscribed.
	 */
	#copies: Copy[] = [];
	/** The cursor start the copies were last bounded at: a lower one asks for a backfill. */
	#start: number;
	/** The newest group handed out: where a new copy is asked to start. */
	#newest?: number;
	/** The last datagram handed out, which never ages until another supersedes it. */
	#datagram?: number;
	/**
	 * Previously handed-out identities, in the order they were superseded. A replacement copy
	 * replays at most its buffered datagrams, so no more than that many are kept, whatever the
	 * budget: a publisher cannot grow it.
	 */
	#datagrams = new Map<number, Instant>();
	/** The newest datagram handed out, which a declared end must stay past. */
	#newestDatagram?: number;
	/** Bumped when this reader subscribes a new copy or a copy's last lease ends, waking its parked reads. */
	#changed = new Signal(0);
	#dispose: Dispose[] = [];
	#closed = false;
	/** A copy declared another end: reads fail with this. */
	#refused?: Error;
	/** The logical track ended cleanly: the reader drains what its copies hold and follows no route. */
	#ended = false;
	/** The serving copy's largest when the track ended, kept once that copy goes. */
	#last?: { largest?: track.Position; object?: track.Position; ended: true };
	/** The first end any copy declared: the logical track's, whichever copy serves. */
	#declared?: track.Position;
	/** The subscriber's record of groups handed out, as the last read passed it. */
	#delivered = new Delivered();
	/** Runs the serving copy gave up and the reader has yet to report; see {@link #tryDrop}. */
	#drops: DropRun[] = [];
	/** Whether groups were ever asked for; until then the reader keeps no replaced copy. */
	#groupsRead = false;
	/** The groups handed out and still read, which other copies may back; see {@link #backs}. */
	readonly out = new Set<Recover>();
	/** Every group handed out and still open, which ends with the reader. */
	#open = new Set<GroupConsumer>();
	/** The soonest deadline a read waits on, armed to wake it then. */
	#wall?: { at: number; timer: AbortController };
	/** Records the properties a copy committed, and which source it came from. */
	#record: (info: track.Info, source: Source) => void;

	/** `parent` is the newest copy of the reader this one forks, if any. */
	constructor(route: Route, shell: ResumeShell, record: (info: track.Info, source: Source) => void, parent?: Copy) {
		this.route = route;
		this.#shell = shell;
		this.#record = record;
		this.#start = shell.cursor.peek().start;
		route.readers.add(this);
		this.#dispose.push(
			shell.subscription.subscribe(() => {
				for (const copy of this.#copies) this.#update(copy);
			}),
			shell.cursor.subscribe(() => {
				for (const copy of this.#copies) this.#bound(copy);
				const passed = this.#start;
				this.#start = shell.cursor.peek().start;
				if (this.#start < passed) this.#backfill(this.#start, passed);
				if (this.#ended) this.#letGo(this.#delivered);
			}),
		);
		// A fork starts on its parent's newest copy, at that copy's generation: the copy keeps its
		// retained groups even where no route serves the track, between routes or once the front
		// concluded.
		if (parent) {
			this.#generation = parent.generation;
			this.#add({
				generation: parent.generation,
				source: parent.source,
				sub: parent.sub.fork(this.#wire({})),
				replaced: false,
				lease: 0,
			});
		}
		// Subscribed to the serving copy at once, so groups it has now are the reader's even if
		// the route changes before the first read, and to each copy after.
		this.sync();
	}

	/** Follow the route: subscribe to a new copy and mark the ones it replaced. */
	sync(): void {
		if (this.#closed || this.#ended) return;
		const route = this.route;
		if (this.#generation === route.generation) return;
		this.#generation = route.generation;
		// What a former route gave up is no verdict on the one that serves now.
		this.#drops = [];
		const source = route.source;
		if (!source) return;
		// The replaced copies keep delivering until the new one delivers its first group: a
		// group they get meanwhile may be one the new route never has. See #observe.
		for (const copy of this.#copies) copy.replaced = true;
		// Cloned handles share closed, so a returning route reuses its subscription and
		// leases rather than replaying its cache and retaining another copy per flap. The newest
		// is the route's own copy, not a backfill of it.
		const index = this.#copies.findLastIndex((copy) => copy.source.from.closed === source.from.closed);
		if (index !== -1) {
			const [copy] = this.#copies.splice(index, 1);
			source.retain();
			copy.source.release();
			copy.source = source;
			copy.generation = route.generation;
			copy.replaced = false;
			copy.until = undefined;
			copy.since = undefined;
			copy.done = undefined;
			copy.datagramsDone = undefined;
			// Resume where a fresh copy would: a group it buffered while replaced, below the newest
			// another route handed out, is not handed out.
			if (this.#newest !== undefined) copy.floor = Math.max(copy.floor ?? 0, this.#newest);
			this.#update(copy);
			this.#bound(copy);
			this.#copies.push(copy);
			this.wake();
			return;
		}
		const floor = this.#newest;
		this.#add({
			generation: route.generation,
			source,
			// The wire request carries the floor from the first message: some sessions read the
			// demand only once.
			sub: subscribe(source.wire, route.name, this.#wire({ floor })),
			floor,
			replaced: false,
			lease: 0,
		});
	}

	/** Read `copy` as the newest one, or at `index`, recording the properties it commits. */
	#add(copy: Copy, index = this.#copies.length): void {
		const { source } = copy;
		source.retain();
		this.#bound(copy);
		this.#copies.splice(index, 0, copy);
		const shell = this.#shell;
		const record = this.#record;
		copy.sub.info().then(
			(info) => {
				record(info, source);
				shell.commit(info);
			},
			() => {},
		);
		copy.sub.finished().then(
			// A copy already let go says nothing more about the track.
			(end) => {
				if (!this.#copies.includes(copy)) return;
				this.#declare(end);
				this.#learnEnd();
			},
			() => {},
		);
		void copy.sub.closed.then(async (closed) => {
			// The serving copy closing cleanly is the logical track's end, once nothing it replaced
			// may still deliver below it.
			if (closed !== null) return;
			for (;;) {
				this.sync();
				if (this.#closed || this.#serving() !== copy || this.#conclude()) return;
				await new Promise<void>((resolve) => {
					const dispose = this.changed(() => {
						dispose();
						resolve();
					});
				});
			}
		});
		this.#changed.update((n) => n + 1);
	}

	/** What `copy` is asked for: the reader's own preferences, from its floor and through its replacement. */
	#wire(copy: Pick<Copy, "floor" | "until">): track.Subscription {
		const preferences = this.#shell.subscription.peek() ?? {};
		const bounds = groupBounds(preferences.groups);
		const start = Math.max(bounds.start, copy.floor ?? 0);
		const replaced = copyEnd(copy);
		const end = replaced === undefined ? bounds.end : Math.min(bounds.end ?? replaced, replaced);
		return {
			...preferences,
			groups: {
				start: start > 0 ? { included: start } : undefined,
				end: end === undefined ? undefined : { excluded: end },
			},
		};
	}

	#update(copy: Copy): void {
		if (copy.sub.closed.peek() === undefined) copy.sub.update(this.#wire(copy));
	}

	/**
	 * Apply the reader's group cursor to `copy`, never below the copy's own resume point: groups
	 * under it were handed out by an earlier route, or never owed.
	 */
	#bound(copy: Copy): void {
		const cursor = this.#shell.cursor.peek();
		const start = Math.max(cursor.start, copy.floor ?? 0);
		const replaced = copyEnd(copy);
		const end = replaced === undefined ? cursor.end : Math.min(cursor.end ?? replaced, replaced);
		copy.sub.withRange({
			start: { included: start },
			end: end === undefined ? undefined : { excluded: end },
		});
	}

	/**
	 * Ask the newest copy's route for the groups from `start` up to `passed` under that copy's
	 * resume point, which a lowered floor newly covers. The copies that cached them may be gone,
	 * and that copy never asked for them, so a bounded copy of the same route asks for just those.
	 */
	#backfill(start: number, passed: number): void {
		const newest = this.#copies.at(-1);
		if (this.#closed || this.#ended || newest?.floor === undefined) return;
		const end = Math.min(passed, newest.floor);
		// An ordered reader never goes back, nor does any reader past its budget.
		if (this.#delivered.covers(start, end)) return;
		// A floor lowered again asks once.
		const closed = newest.source.from.closed;
		const asked = (copy: Copy) =>
			copy.source.from.closed === closed &&
			copy.floor !== undefined &&
			copy.floor <= start &&
			(copyEnd(copy) ?? Number.POSITIVE_INFINITY) >= end;
		if (this.#copies.some(asked)) return;
		const backfill = { floor: start, until: end - 1 };
		this.#add(
			{
				generation: newest.generation,
				source: newest.source,
				sub: subscribe(newest.source.wire, this.route.name, this.#wire(backfill)),
				...backfill,
				// Bounded from the start: everything it may deliver is older than what the reader
				// already read, so it is waited for no longer than the reader's budget.
				replaced: true,
				since: now(),
				lease: 0,
			},
			this.#copies.length - 1,
		);
	}

	/** Wake parked reads, so they look again and let go of what nothing needs. */
	wake(): void {
		this.#changed.update((n) => n + 1);
	}

	/** The reader's copy of the route at `generation`, if it subscribed one, rather than a backfill of it. */
	copy(generation: number): Copy | undefined {
		return this.#copies.findLast((copy) => copy.generation === generation);
	}

	/** The copies other than `except` whose track still holds group `sequence` unfailed, newest first. */
	holding(sequence: number, except: Copy): Copy[] {
		const holding: Copy[] = [];
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			if (copy !== except && hooks.holdsGroup(copy.sub, sequence)) holding.push(copy);
		}
		return holding;
	}

	cachedGroup(sequence: number, from = 0): GroupConsumer | undefined {
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			const group = hooks.cachedGroup(copy.sub, sequence, from);
			if (group) return this.#handOut(copy, group);
		}
		return undefined;
	}

	/**
	 * `copy` handed out `sequence`. The serving copy's first group bounds the copies it replaced
	 * just below it: what they hold under it may be groups the new route never has, and what
	 * is at or above it the new route delivers. Never below a group already handed out. A group
	 * it replays, already handed out, bounds nothing: the replaced copies may still hold the next.
	 * Nor does a backfill of the serving route.
	 */
	#observe(copy: Copy, sequence: number): void {
		if (copy.replaced || copy.generation !== this.#generation) return;
		const bound = Math.max(this.#newest ?? -1, sequence - 1);
		for (const replaced of this.#copies) {
			if (replaced === copy || !replaced.replaced || replaced.until !== undefined) continue;
			replaced.until = bound < 0 ? null : bound;
			replaced.since = now();
			this.#update(replaced);
			this.#bound(replaced);
		}
	}

	#deliverable(copy: Copy, sequence: number, delivered: Delivered): boolean {
		const end = copyEnd(copy);
		return (end === undefined || sequence < end) && !delivered.has(sequence);
	}

	tryGroup(ordered: boolean, delivered: Delivered, dispositions: boolean): Recv {
		this.#delivered = delivered;
		this.sync();
		// Before anything is handed out, so a copy claiming other content hands out none of it.
		this.#learnEnd();
		if (this.#closed) return this.#refused ? { kind: "error", error: this.#refused } : { kind: "done" };
		this.#groupsRead = true;
		if (dispositions) {
			const dropped = this.#tryDrop(delivered);
			if (dropped) return dropped;
		}
		// The lowest group any copy holds next, in either order: a replaced copy's buffered group
		// comes before a later one the serving copy already has. Arrival order still delivers a
		// lower group that truly arrives later.
		const next = this.#lowest(ordered, delivered);
		if (next) {
			const recv = take(next, ordered);
			if (recv.kind === "group") {
				this.#observe(next, recv.group.sequence);
				return { kind: "group", group: this.#handOut(next, recv.group) };
			}
		}
		// The serving copy first: it is the one that keeps delivering.
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			if (!this.#feeds(copy)) continue;
			for (;;) {
				const recv = take(copy, ordered);
				if (recv.kind === "group") {
					if (!this.#deliverable(copy, recv.group.sequence, delivered)) {
						recv.group.close();
						continue;
					}
					this.#observe(copy, recv.group.sequence);
					return { kind: "group", group: this.#handOut(copy, recv.group) };
				}
				// Drained: the track ended as the copy did, which is not cleanly when its
				// session went away short of the declared end.
				if (recv.kind === "done") copy.done = copy.sub.closed.peek() ?? null;
				else if (recv.kind === "error") copy.done = recv.error;
				break;
			}
		}
		this.#letGo(delivered);
		// A copy that ran out or was let go no longer holds back a drop.
		if (dispositions) {
			const dropped = this.#tryDrop(delivered);
			if (dropped) return dropped;
		}
		return this.#end((copy) => copy.done);
	}

	tryDrop(delivered: Delivered): (Recv & { kind: "drop" }) | undefined {
		this.#delivered = delivered;
		this.sync();
		this.#learnEnd();
		return this.#tryDrop(delivered);
	}

	/**
	 * The next run the serving copy gave up that `delivered` lacks and no other copy may still
	 * deliver. Only the serving copy's verdicts count; a copy it replaced may still deliver what
	 * is under its bound, so a run there waits until that copy no longer feeds. Mirrors the
	 * Rust `Reader::poll_drop`.
	 */
	#tryDrop(delivered: Delivered): (Recv & { kind: "drop" }) | undefined {
		if (this.#closed) return undefined;
		const serving = this.#serving();
		if (serving) {
			for (let dropped = hooks.tryDrop(serving.sub); dropped; dropped = hooks.tryDrop(serving.sub)) {
				// A route failing is no verdict: another route may still deliver the group, and
				// one it failed while buffered is handed out to continue through its recovery.
				if (routeFailed(dropped.error)) continue;
				this.#drops = insertDrop(this.#drops, dropped.start, dropped.end, dropped.error);
			}
		}
		this.#drops = this.#drops.filter((run) => !delivered.covers(run.start, run.end));
		const cursor = this.#shell.cursor.peek();
		const feeding: Run[] = [];
		for (const copy of this.#copies) {
			if (copy === serving || !this.#feeds(copy)) continue;
			const start = Math.max(cursor.start, copy.floor ?? 0);
			insertRun(feeding, start, copyEnd(copy) ?? Number.POSITIVE_INFINITY);
		}
		for (const run of this.#drops) {
			const end = Math.min(run.end, cursor.end ?? run.end);
			for (const gap of delivered.gaps(Math.max(run.start, cursor.start), end)) {
				const [open] = runGaps(feeding, gap.start, gap.end);
				if (open) {
					// Report once without marking it delivered: a late fill may still land.
					this.#drops = removeRun(this.#drops, open.start, open.end);
					return { kind: "drop", ...open, error: run.error };
				}
			}
		}
		return undefined;
	}

	// Whether `copy` may still feed groups: its channel has not run out, and what it holds is
	// within the reader's budget, which ends a full budget after a newer group superseded it.
	#feeds(copy: Copy): boolean {
		return copy.done === undefined && !this.#expired(copy, this.#budget());
	}

	// The copy whose next group is the lowest deliverable one, dropping any it holds next that
	// are not.
	#lowest(ordered: boolean, delivered: Delivered): Copy | undefined {
		let lowest: { copy: Copy; sequence: number } | undefined;
		for (const copy of this.#copies) {
			if (!this.#feeds(copy)) continue;
			for (;;) {
				const sequence = hooks.peekGroup(copy.sub, ordered);
				if (sequence === undefined) break;
				if (this.#deliverable(copy, sequence, delivered)) {
					// A tie goes to the newer copy, so the serving one hands out its first group and
					// bounds the copies it replaced.
					if (!lowest || sequence <= lowest.sequence) lowest = { copy, sequence };
					break;
				}
				const recv = take(copy, ordered);
				if (recv.kind !== "group") break;
				recv.group.close();
			}
		}
		return lowest?.copy;
	}

	tryDatagram(delivered: Delivered): RecvDatagram {
		this.#delivered = delivered;
		this.sync();
		this.#learnEnd();
		if (this.#closed) return this.#refused ? { kind: "error", error: this.#refused } : { kind: "done" };
		// Like groups, the last delivery stays fresh; earlier identities cost memory only
		// while the reader's budget still covers their successor's arrival.
		const at = now();
		const budget = this.#budget() * 1_000_000;
		for (const [sequence, since] of this.#datagrams) {
			if (at - since < budget) break;
			this.#datagrams.delete(sequence);
		}
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			// A replaced copy's datagrams are the serving one's to deliver.
			if (copy.datagramsDone !== undefined || copy.replaced) continue;
			for (;;) {
				const recv = hooks.tryRecvDatagram(copy.sub);
				if (recv.kind === "datagram") {
					const sequence = recv.datagram.sequence;
					if (sequence === this.#datagram || this.#datagrams.has(sequence)) continue;
					if (this.#datagram !== undefined) {
						if (this.#datagrams.size === MAX_DATAGRAMS) {
							this.#datagrams.delete(this.#datagrams.keys().next().value as number);
						}
						this.#datagrams.set(this.#datagram, at);
					}
					this.#datagram = sequence;
					this.#newestDatagram = Math.max(this.#newestDatagram ?? 0, sequence);
					return recv;
				}
				if (recv.kind === "done") copy.datagramsDone = copy.sub.closed.peek() ?? null;
				else if (recv.kind === "error") copy.datagramsDone = recv.error;
				break;
			}
		}
		this.#letGo(delivered);
		const end = this.#end((copy) => copy.datagramsDone);
		if (end.kind === "error" || end.kind === "done") return end;
		return { kind: "idle" };
	}

	/**
	 * Whether the track ended, once the serving copy's channel (as `done` reads it) ran out:
	 * cleanly, or with a verdict a route failure does not excuse.
	 */
	#end(done: (copy: Copy) => Error | null | undefined): Recv & { kind: "idle" | "done" | "error" } {
		const route = this.route;
		const last = this.#copies.at(-1);
		const serving = last?.generation === this.#generation ? last : undefined;
		const ended = serving ? done(serving) : undefined;
		// The serving copy drained a complete track: that is its end, whichever route would
		// serve it next. Once the track ended, a copy let go held nothing more.
		if (serving ? ended === null : this.#ended) return this.#conclude() ? { kind: "done" } : { kind: "idle" };
		if (ended instanceof Error) {
			// A refusal from a route that still serves is the track's answer.
			if (!routeFailed(ended)) return this.#fail(ended);
			if (route.refusal) return this.#fail(route.refusal);
		}
		// Refused before any copy fed it: nothing will.
		if (!last && route.refusal) return this.#fail(route.refusal);
		if (route.concluded) {
			// No front is left to replace the copy: the track ends as it does.
			if (!serving) return this.#fail(dropped());
			if (ended instanceof Error) return this.#fail(ended);
		}
		return { kind: "idle" };
	}

	#fail(error: Error): Recv & { kind: "error" } {
		this.#shell.close(error);
		return { kind: "error", error };
	}

	/**
	 * End the logical track cleanly, once its serving copy did and nothing it replaced may still
	 * deliver a group below that end: settle `closed` and leave the route, keeping each copy only
	 * while it holds something left to read. The other channel's backlog stays readable. Whether
	 * the track ended.
	 */
	#conclude(): boolean {
		if (this.#closed) return false;
		if (this.#ended) return true;
		// Learned before any copy that knows it goes.
		this.#learnEnd();
		const serving = this.#serving();
		if (serving) this.#boundAtEnd(serving);
		if (this.#letGo(this.#delivered) || this.#closed) return false;
		this.#ended = true;
		this.#last = {
			largest: serving && hooks.peekLargest(serving.sub).largest,
			object: serving && hooks.objectLargest(serving.sub),
			ended: true,
		};
		this.#shell.close();
		this.route.leave(this);
		this.#letGo(this.#delivered);
		this.wake();
		return true;
	}

	/**
	 * Drop the copies nothing needs any more. A replaced one goes by {@link #retains}. One not yet
	 * replaced stays, even between routes: it is still the newest copy there is, until the track
	 * ended and it holds nothing left to read. Whether a group the reader lacks may still land on
	 * a replaced one, which a clean end waits for.
	 */
	#letGo(delivered: Delivered): boolean {
		// An end a copy declared outlives the copy.
		this.#learnEnd();
		if (this.#closed) return false;
		const last = this.#copies.at(-1);
		const edge = last && hooks.latestSequence(last.sub);
		const cursor = this.#shell.cursor.peek();
		let waits = false;
		this.#copies = this.#copies.filter((copy) => {
			if (!copy.replaced) {
				if (!this.#ended || copy.lease > 0) return true;
				const start = Math.max(cursor.start, copy.floor ?? 0);
				if (!hooks.spent(copy.sub, start, undefined, delivered)) return true;
				// A group read from a copy it replaced may still continue from it.
				if (this.#backs(copy, true, grace(this.#budget()))) return true;
			} else {
				const retains = this.#retains(copy, delivered, edge);
				if (retains === "wait") waits = true;
				if (retains) return true;
			}
			this.#drop(copy);
			return false;
		});
		return waits;
	}

	/**
	 * How the reader still needs replaced `copy`: `"wait"` while a group the reader lacks may still
	 * land on it, `"keep"` while it holds one unread or backs a group handed out. `edge` is the
	 * newest group the serving copy has.
	 */
	#retains(copy: Copy, delivered: Delivered, edge: number | undefined): "wait" | "keep" | undefined {
		// Datagrams are ephemeral, so only groups keep a copy: a reader that only reads datagrams
		// keeps none, whatever its budget.
		if (!this.#groupsRead) return undefined;
		// Not yet bounded, it may still feed a group the new copy never has.
		if (copy.until === undefined && copy.done === undefined) return "wait";
		const end = copyEnd(copy);
		// Superseded once the serving copy has a group past the bound.
		if (copy.since === undefined && end !== undefined && edge !== undefined && edge >= end) copy.since = now();
		const budget = this.#budget();
		const wait = grace(budget);
		// What is under the bound is past the reader's budget a full budget after a newer group
		// superseded it, landed or not.
		if (copy.done === undefined && end !== undefined && !this.#expired(copy, budget)) {
			const cursor = this.#shell.cursor.peek();
			// From where it was asked to start, or else from what it first got.
			const start = Math.max(cursor.start, copy.floor ?? 0);
			const floor = copy.floor !== undefined;
			// A group still to land is waited for once something superseded it, as a gap is.
			const land = { start, end, floor, grace: wait, delivered };
			if (copy.since !== undefined && hooks.mayLand(copy.sub, land)) return "wait";
			if (hooks.hasUnreadGroup(copy.sub, cursor.start, end, delivered)) return "keep";
		}
		if (copy.lease > 0) return "keep";
		const lands = copy.since !== undefined && !this.#expired(copy, wait);
		return this.#backs(copy, lands, wait) ? "keep" : undefined;
	}

	/**
	 * Whether `copy` holds a group still read, or may yet get it within `wait` when it `lands`: a
	 * failure of the copy that group is read from continues here.
	 */
	#backs(copy: Copy, lands: boolean, wait: Milli): boolean {
		for (const recover of this.out) {
			const sequence = recover.sequence;
			if (hooks.holdsGroup(copy.sub, sequence)) return true;
			// A group at or past where it was asked to start may land even before its first
			// arrival; without a floor, only what follows its first arrival can.
			const asked = copy.floor !== undefined && copy.floor <= sequence;
			const land = { start: sequence, end: sequence + 1, floor: asked, grace: wait };
			if (lands && hooks.mayLand(copy.sub, land)) return true;
		}
		return false;
	}

	/**
	 * Bound every replaced copy not yet bounded at the end of `serving`, which ended cleanly, so a
	 * group it still carries below the end is waited for and nothing past it.
	 */
	#boundAtEnd(serving: Copy): void {
		const declared = this.#declared;
		const until =
			declared !== undefined
				? finalSequence(declared) - 1
				: Math.max(this.#newest ?? -1, hooks.latestSequence(serving.sub) ?? -1);
		for (const copy of this.#copies) {
			if (!copy.replaced || copy.until !== undefined) continue;
			copy.until = until < 0 ? null : until;
			copy.since = now();
			this.#update(copy);
			this.#bound(copy);
		}
	}

	/** The reader's budget: how far behind a group may fall before it is skipped. */
	#budget(): Milli {
		return this.#shell.subscription.peek()?.maxDelay ?? Milli.zero;
	}

	/**
	 * Whether `within` passed since `copy` was superseded; see {@link Copy.since}. Arms a wakeup
	 * for then, so a read waiting on the copy looks again.
	 */
	#expired(copy: Copy, within: Milli): boolean {
		if (copy.since === undefined) return false;
		// Plain arithmetic: an unenforced budget is infinite, which no duration brand holds.
		const at = copy.since + within * 1_000_000;
		if (now() >= at) return true;
		this.#wakeAt(at);
		return false;
	}

	// Wake parked reads at `at`, unless a wakeup is already armed sooner.
	#wakeAt(at: number): void {
		if (this.#closed || !Number.isFinite(at) || (this.#wall && this.#wall.at <= at)) return;
		this.#wall?.timer.abort();
		const timer = new AbortController();
		this.#wall = { at, timer };
		// A wakeup is never a reason to hold a Node/Bun process open.
		void sleepUntil(Instant(at), { signal: timer.signal, ref: false }).then(
			() => {
				if (timer.signal.aborted) return;
				this.#wall = undefined;
				this.wake();
			},
			() => {},
		);
	}

	#drop(copy: Copy, abort?: Error): void {
		copy.sub.close(abort);
		copy.source.release();
	}

	#handOut(copy: Copy, group: GroupConsumer): GroupConsumer {
		this.#newest = Math.max(this.#newest ?? 0, group.sequence);
		// Another copy holding the group, now or once it lands there, is where a failure continues
		// it, so it stays while the group is read; see #backs. Recorded even with one copy, since a
		// later route's copy may come to hold it.
		const recover = new Recover(this, group, copy);
		hooks.recoverGroup(group, recover);
		recover.watch(group);
		// Closed with the reader, as a direct reader's close reaches the groups it handed out.
		this.#open.add(group);
		void group.closed.then(() => this.#open.delete(group));
		return group;
	}

	/** The logical track ended cleanly, so no route continues it. */
	get ended(): boolean {
		return this.#ended;
	}

	/** A group handed out ended or moved copies: let go of what nothing needs any more. */
	freed(): void {
		// Once the track ended, no read may come to let go of a copy.
		if (this.#ended) this.#letGo(this.#delivered);
		// A parked read lets go of a replaced copy nothing needs any more.
		this.wake();
	}

	changed(fn: () => void): Dispose {
		let fired = false;
		const disposes: Dispose[] = [];
		const dispose = () => {
			fired = true;
			for (const close of disposes) close();
		};
		const once = () => {
			if (fired) return;
			dispose();
			fn();
		};
		disposes.push(this.route.changed.changed(once), this.#changed.changed(once));
		for (const copy of this.#copies)
			disposes.push(hooks.groupChanged(copy.sub, once), hooks.datagramChanged(copy.sub, once));
		return dispose;
	}

	forgetPastBudget(delivered: Delivered): void {
		const newest = this.#copies.at(-1);
		if (newest) hooks.forgetPastBudget(newest.sub, delivered);
	}

	latest(): number | undefined {
		let latest: number | undefined;
		for (const copy of this.#copies) {
			const sequence = hooks.latestSequence(copy.sub);
			if (sequence !== undefined && (latest === undefined || sequence > latest)) latest = sequence;
		}
		return latest;
	}

	#serving(): Copy | undefined {
		const last = this.#copies.at(-1);
		return last?.generation === this.#generation ? last : undefined;
	}

	// The logical track's end, learning it from any copy that declared one. A group or datagram
	// at or past it, on any copy or already handed out, claims other content: a direct track
	// refuses an end below what it received.
	#learnEnd(): track.Position | undefined {
		for (const copy of this.#copies) {
			const end = hooks.subscriberEnd(copy.sub);
			if (end) this.#declare(end);
		}
		const declared = this.#declared;
		if (declared && !this.#closed) {
			const final = finalSequence(declared);
			const seen = (sequence: number | undefined) => sequence !== undefined && sequence >= final;
			if (
				seen(this.#newest) ||
				seen(this.#newestDatagram) ||
				// The largest counts datagrams too, which the latest group does not.
				this.#copies.some(
					(copy) => seen(hooks.latestSequence(copy.sub)) || seen(hooks.peekLargest(copy.sub).largest?.group),
				)
			)
				this.#refuse();
		}
		return this.#declared;
	}

	// Record `end` as the logical track's end. One logical track has one end, as it has one set
	// of properties: a copy declaring another is refused. A mid-group end refines the group
	// boundary it falls just before, as a wire end may.
	#declare(end: track.Position): void {
		if (this.#closed) return;
		const declared = this.#declared;
		if (!declared) {
			this.#declared = end;
			this.wake();
		} else if (declared.group === end.group && declared.frame === end.frame) {
			return;
		} else if (finalSequence(declared) === finalSequence(end) && (declared.frame === 0 || end.frame === 0)) {
			if (declared.frame === 0) this.#declared = end;
		} else {
			this.#refuse();
		}
	}

	// Stop reading the copies, which claim other content. `closed` settles with the refusal,
	// unless it already settled cleanly.
	#refuse(): void {
		this.#refused = new Incompatible(this.route.name);
		this.#shell.close(this.#refused);
		this.close(this.#refused);
	}

	start(): number | "pending" | undefined {
		this.sync();
		const serving = this.#serving();
		return serving && hooks.sourceStart(serving.sub);
	}

	end(): track.Position | undefined {
		this.sync();
		return this.#learnEnd();
	}

	async finished(): Promise<track.Position> {
		for (;;) {
			this.sync();
			const end = this.#learnEnd();
			if (end) return end;
			const serving = this.#serving();
			const failed = serving?.sub.closed.peek();
			// A copy that failed is only the end of the track if no front replaces it, and a
			// refused replacement leaves the refusal as the answer.
			if (failed instanceof Error) {
				if (!routeFailed(failed)) throw failed;
				if (this.route.refusal) throw this.route.refusal;
				if (this.route.concluded) throw failed;
			}
			if (!serving && this.route.concluded) throw dropped();
			const closed = this.#shell.closed.peek();
			if (closed instanceof Error) throw closed;
			if (closed !== undefined) throw new Error("track closed before its end was known");
			await new Promise<void>((resolve) => {
				const dispose = this.changed(() => {
					dispose();
					resolve();
				});
			});
		}
	}

	bufferedLargest(): track.Position | undefined {
		this.sync();
		const serving = this.#serving();
		return serving && hooks.bufferedLargest(serving.sub);
	}

	objectLargest(): track.Position | undefined {
		this.sync();
		const serving = this.#serving();
		return serving ? hooks.objectLargest(serving.sub) : this.#last?.object;
	}

	peekLargest(): { largest?: track.Position; ended: boolean } {
		this.sync();
		const serving = this.#serving();
		return serving ? hooks.peekLargest(serving.sub) : (this.#last ?? { ended: false });
	}

	async largest(seen: track.Position | undefined): Promise<track.Position | undefined> {
		for (;;) {
			this.sync();
			const serving = this.#serving();
			const view = serving ? hooks.peekLargest(serving.sub) : this.#last;
			const largest = view?.largest;
			const { start, end } = this.#shell.cursor.peek();
			// A replacement copy can start below what an earlier one reported.
			if (
				largest &&
				largest.group >= start &&
				(end === undefined || largest.group < end) &&
				(!seen || largest.group > seen.group || (largest.group === seen.group && largest.frame > seen.frame))
			)
				return largest;
			// A copy failing is its route failing: the logical track's stream ends only with the
			// route, or once the serving copy reaches its end cleanly.
			if (this.#shell.closed.peek() !== undefined) return undefined;
			if (view?.ended && !(serving?.sub.closed.peek() instanceof Error)) return undefined;
			if ((this.route.concluded || this.route.refusal) && (!serving || view?.ended)) return undefined;
			await new Promise<void>((resolve) => {
				const disposes: Dispose[] = [];
				const wake = () => {
					for (const dispose of disposes) dispose();
					resolve();
				};
				disposes.push(this.changed(wake), this.#shell.closed.changed(wake), this.#shell.cursor.changed(wake));
				if (serving) disposes.push(hooks.largestChanged(serving.sub, wake));
			});
		}
	}

	join(): void {
		this.sync();
		const serving = this.#serving();
		if (serving) hooks.joinSubscriber(serving.sub);
	}

	fork(options?: track.Subscription): track.Subscriber {
		this.sync();
		const newest = this.#copies.at(-1);
		return hooks.resumeSubscriber(
			this.route.name,
			options,
			(shell) => new Reader(this.route, shell, this.#record, newest),
		);
	}

	close(abort?: Error): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#wall?.timer.abort();
		this.#wall = undefined;
		for (const dispose of this.#dispose) dispose();
		this.#dispose = [];
		// A recovery in flight settles with the group rather than adopting a replacement.
		for (const group of this.#open) hooks.releaseGroupReader(group, abort);
		this.#open.clear();
		for (const copy of this.#copies) this.#drop(copy, abort);
		this.#copies = [];
		this.route.leave(this);
		// A read still waiting on the reader looks again and finds it closed.
		this.wake();
	}
}

/** A fetch of a group's rest from one route generation. */
interface Fetch {
	generation: number;
	/** Set once the fetch settles. */
	outcome?: { group: GroupConsumer } | { error: Error };
	/** Fires once it settles. */
	settled: Signal<boolean>;
	/** Abandon it, closing a group it returns, unless the caller kept that group. */
	cancel(): void;
}

function startFetch(
	fetchGroup: NonNullable<BroadcastWire["fetchGroup"]>,
	name: string,
	sequence: number,
	generation: number,
	signal: AbortSignal,
): Fetch {
	// Its own controller, so a fetch a later route supersedes stops without ending the recovery.
	const controller = new AbortController();
	const onAbort = () => controller.abort(signal.reason);
	signal.addEventListener("abort", onAbort, { once: true });
	const fetch: Fetch = {
		generation,
		settled: new Signal(false),
		cancel: () => {
			signal.removeEventListener("abort", onAbort);
			controller.abort();
			if (fetch.outcome && "group" in fetch.outcome) fetch.outcome.group.close();
		},
	};
	fetchGroup(name, sequence, { signal: controller.signal })
		.then(
			(group) => {
				// A copy that already failed continues nothing: that is the route failing again.
				const failed = group.closed.peek();
				if (failed instanceof Error) {
					group.close();
					fetch.outcome = { error: failed };
					return;
				}
				fetch.outcome = { group };
				// Abandoned while it was out: nobody reads it.
				if (controller.signal.aborted) group.close();
			},
			(err: unknown) => {
				fetch.outcome = { error: err instanceof Error ? err : new Error(String(err)) };
			},
		)
		.finally(() => {
			signal.removeEventListener("abort", onAbort);
			fetch.settled.set(true);
		});
	return fetch;
}

/** Wait for `change`, or throw once `signal` aborts, leaving no listener behind either way. */
async function until(change: PromiseLike<unknown>, signal?: AbortSignal): Promise<void> {
	signal?.throwIfAborted();
	if (!signal) {
		await race([change]);
		return;
	}
	let onAbort = () => {};
	const abort = new Promise<void>((resolve) => {
		onAbort = resolve;
		signal.addEventListener("abort", onAbort, { once: true });
	});
	try {
		await race([change, abort]);
	} finally {
		signal.removeEventListener("abort", onAbort);
	}
	signal.throwIfAborted();
}

function dropped(): Error {
	return new StreamError(StreamCode.Cancel, { message: "no route serves the track" });
}

/** Carries a group across route changes; see the module docs. */
class Recover implements GroupRecover {
	#reader: Reader;
	readonly sequence: number;
	/** The copy being read, whose subscription this group keeps alive while it is read. */
	#copy?: Copy;

	/**
	 * Puts the group on the reader's list of those still read, so the other copies that hold it,
	 * or may yet get it, stay for a failure to continue from.
	 */
	constructor(reader: Reader, group: GroupConsumer, copy: Copy) {
		this.#reader = reader;
		this.sequence = group.sequence;
		this.#copy = copy;
		copy.lease++;
		reader.out.add(this);
	}

	/**
	 * Release the copy once the group ends. Called only after the group carries this recovery:
	 * reading `closed` before then would settle it on a failure recovery could still continue.
	 */
	watch(group: GroupConsumer): void {
		void group.closed.then(() => this.close());
	}

	close(): void {
		this.#release(this.#reader.out.delete(this));
	}

	// Release the copy being read. `wake` says a parked read may let go of a copy anyway.
	#release(wake = false): void {
		const copy = this.#copy;
		this.#copy = undefined;
		const freed = copy !== undefined && --copy.lease === 0;
		if (wake || freed) this.#reader.freed();
	}

	get changed(): Getter<unknown> {
		return this.#reader.route.changed;
	}

	wants(failed: Error | undefined): boolean {
		const copy = this.#copy;
		if (!copy) return false;
		if (failed && this.#undelivered(failed, copy)) return true;
		const route = this.#reader.route;
		return route.generation !== copy.generation || route.concluded;
	}

	/**
	 * Whether `copy` failed delivering the group rather than judging it, so its own route may be
	 * asked again. Any other failure would repeat on every copy from that route, so only another
	 * route is asked, which bounds a hostile route to one bad answer per route change.
	 */
	#undelivered(failed: Error, copy: Copy): boolean {
		return routeFailed(failed) || copy.sub.closed.peek() instanceof Error;
	}

	async replace(index: number, failed: Error | undefined, signal: AbortSignal): Promise<Replacement> {
		let fetch: Fetch | undefined;
		let kept: GroupConsumer | undefined;
		try {
			const replacement = await this.#replace({ index, failed, signal }, (next) => {
				fetch?.cancel();
				fetch = next;
			});
			kept = replacement.group;
			return replacement;
		} finally {
			// A fetch nothing continues from is abandoned, along with whatever it returns.
			if (fetch?.outcome === undefined || !("group" in fetch.outcome) || fetch.outcome.group !== kept)
				fetch?.cancel();
		}
	}

	async #replace(
		{ index, failed, signal }: { index: number; failed?: Error; signal: AbortSignal },
		started: (fetch: Fetch) => void,
	): Promise<Replacement> {
		const reader = this.#reader;
		const route = reader.route;
		let fetch: Fetch | undefined;
		for (;;) {
			signal.throwIfAborted();
			// The serving subscription may still deliver a group its route refused to fetch.
			let landing: PromiseLike<unknown> | undefined;
			let unwatch: Dispose | undefined;
			// Subscribe the reader to a new route from its newest group, which is this one or
			// later, so that route's subscription may deliver the rest of it.
			reader.sync();
			const current = this.#copy;
			if (!current) throw new Error("group closed");
			// The front refused the route that would continue it: nothing will.
			if (failed && route.refusal) throw route.refusal;
			const generation = route.generation;
			const serving = route.source ? reader.copy(generation) : undefined;
			const waits: Getter<unknown>[] = [route.changed];
			// A failed group continues from any copy still holding it, newest first: its name fixes
			// its content. A stall only moves to the serving copy, so two stalled copies never trade
			// the group back and forth.
			if (failed) {
				const held = reader.holding(this.sequence, current)[0];
				const cached = held && hooks.cachedGroup(held.sub, this.sequence, index);
				if (cached) return this.#replacement(cached, held);
				// Once the track ended, no route continues it: only a copy holding it could.
				if (reader.ended) throw failed;
				// A verdict would repeat from the route that gave it.
				if (generation === current.generation && !this.#undelivered(failed, current)) throw failed;
			}
			if (!serving) {
				// Nothing serves: a failed copy waits for a route, unless none will come.
				if (failed && route.concluded) throw failed;
			} else if (generation !== current.generation || failed) {
				// The serving route gave the group up, which ends it with that route's error however
				// a fetch of the rest would answer. A copy that failed dropped its open groups with
				// it, and a group the route failed is no verdict either.
				const given = hooks.givenUp(serving.sub, this.sequence);
				if (given.error && !routeFailed(given.error) && !(serving.sub.closed.peek() instanceof Error)) {
					throw given.error;
				}
				waits.push(given.changed);
				// A newer copy's track caches it: continue from there.
				if (!failed && generation !== current.generation) {
					const cached = hooks.cachedGroup(serving.sub, this.sequence, index);
					if (cached) return this.#replacement(cached, serving);
				}
				// Otherwise ask for it, if the route's version can fetch. The subscription may still
				// deliver it first. Without FETCH only a copy continues a failed group, and none
				// holds it, so it ends as the failed copy left it.
				const fetchGroup = serving.source.wire.fetchGroup;
				if (!fetchGroup && failed) throw failed;
				if (fetchGroup && fetch?.generation !== generation) {
					fetch = startFetch(fetchGroup, route.name, this.sequence, generation, signal);
					started(fetch);
				}
				// A fetch from an earlier route answers for that route, not this one.
				const asked = fetch?.generation === generation ? fetch : undefined;
				const stale = hooks.staleAt(serving.sub, this.sequence);
				if (stale.expired()) throw new StreamError(StreamCode.Old, { message: "group is past the budget" });
				waits.push(...stale.changed, ...(asked && !asked.outcome ? [asked.settled] : []));
				const outcome = asked?.outcome;
				if (outcome && "group" in outcome) return this.#replacement(outcome.group, serving);
				// That route failing too waits for the next one, and a refusal is a verdict only for
				// a copy that failed, once the serving subscription will not deliver the group either.
				if (outcome && failed && !routeFailed(outcome.error) && !(serving.sub.closed.peek() instanceof Error)) {
					const maxDelay = serving.sub.subscription.peek()?.maxDelay ?? Milli.zero;
					const land = { start: this.sequence, end: this.sequence + 1, floor: true, grace: grace(maxDelay) };
					if (generation === current.generation || !hooks.mayLand(serving.sub, land)) throw outcome.error;
					landing = new Promise<void>((resolve) => {
						unwatch = hooks.groupChanged(serving.sub, resolve);
					});
				}
			}
			try {
				const changed = Signal.race(...waits);
				await until(landing === undefined ? changed : race([changed, landing]), signal);
			} finally {
				unwatch?.();
			}
		}
	}

	#replacement(group: GroupConsumer, copy: Copy): Replacement {
		return {
			group,
			expiry: hooks.staleAt(copy.sub, this.sequence),
			adopt: () => {
				// A later switch must keep this copy subscribed while the recovered group still
				// reads it, just like a group handed out from that copy directly. Leased first, so
				// letting go of the copies it leaves never drops it.
				copy.lease++;
				this.#release();
				this.#copy = copy;
			},
			discard: () => group.close(),
		};
	}
}

/**
 * One broadcast served through one route at a time; see the module docs.
 *
 * Its {@link consumer} is the broadcast handed out: every track read through it follows the
 * front's route. The origin calls {@link serve} as routes of the same epoch take over, and
 * {@link conclude} once none will: readers then follow the last copy to its end.
 */
export class Front {
	/** The logical broadcast; closing its last handle concludes the front. */
	readonly consumer: broadcast.Consumer;
	#source?: Source;
	#routes = new Map<string, Route>();
	/** Each track's properties as first committed, and the source that said so: every later copy must match. */
	#info = new Map<string, { info: track.Info; source: Source }>();
	/** Bumped whenever the source changes, so an answer from a replaced one is dropped. */
	#changed = new Signal(0);
	/** Aborted once the current source is replaced, retiring what was asked of it. */
	#retire = new AbortController();
	#concluded = false;
	/** The tracks each source was checked against the recorded properties for; see #verify. */
	#verified = new WeakMap<Source, Set<string>>();
	/** Whether any source it served came from a session, kept between routes. */
	#fromSession = false;

	constructor() {
		this.consumer = makeFrontConsumer(this);
		void this.consumer.closed.then(() => this.conclude());
		void this.#list(wireOf(this.consumer).lister());
	}

	/**
	 * Answer listings of the front's broadcast with its serving source's listing, while anyone
	 * lists it. A source of the same instance lists the same tracks, so a takeover shows readers
	 * only what differs, once the new source's listing synced.
	 */
	async #list(lister: Lister): Promise<void> {
		const closed = this.consumer.closed;
		// What the front lists, the source being read, and its pending next event.
		let listing:
			| {
					listed: Set<string>;
					/** Names an earlier source listed that the current one has yet to list. */
					stale: Set<string>;
					source?: Source;
					reader?: Tracks;
					next?: Promise<{ event?: TracksEvent; err?: Error }>;
			  }
			| undefined;
		const wait = () => race([Signal.race(lister.used, this.#changed), closed]);
		const stop = () => {
			listing?.reader?.close();
			listing = undefined;
		};
		const read = (reader: Tracks): Promise<{ event?: TracksEvent; err?: Error }> =>
			reader.next().then(
				(event) => ({ event }),
				(err: unknown) => ({ err: err instanceof Error ? err : new Error(String(err)) }),
			);
		// Forwarded as the source reported it, finished or not.
		const unlist = (
			listed: Set<string>,
			stale: Set<string>,
			event: { kind: "removed" | "finished"; name: string },
		) => {
			stale.delete(event.name);
			if (!listed.delete(event.name)) return;
			if (event.kind === "finished") lister.finish(event.name);
			else lister.remove(event.name);
		};
		try {
			for (;;) {
				if (closed.peek() !== undefined) return;
				if (!lister.used.peek()) {
					if (listing) {
						stop();
						lister.clear();
					}
					await wait();
					continue;
				}
				if (this.#concluded) {
					// No route will serve it again, so nothing more will be listed. A source that
					// closed ended its listing after the tracks it finished last: those go first.
					while (listing?.reader && listing.source?.from.closed.peek() !== undefined) {
						const { event } = await (listing.next ?? read(listing.reader));
						listing.next = undefined;
						if (!event) break;
						if (event.kind === "removed" || event.kind === "finished")
							unlist(listing.listed, listing.stale, event);
					}
					stop();
					lister.end();
					continue;
				}
				listing ??= { listed: new Set(), stale: new Set() };
				if (listing.source !== this.#source) {
					listing.reader?.close();
					listing.source = this.#source;
					listing.stale = new Set(listing.listed);
					listing.next = undefined;
					try {
						listing.reader = this.#source?.wire.tracks();
					} catch (err) {
						// A closing source lists nothing more; the front moves off it soon.
						listing.reader = undefined;
						// A live one refuses: served through a route claiming a prefix, it names
						// nothing to list. A later reader asks again.
						if (listing.source && listing.source.from.closed.peek() === undefined) {
							stop();
							lister.abort(err instanceof Error ? err : new Error(String(err)));
							continue;
						}
					}
				}
				const reader = listing.reader;
				if (!reader) {
					await wait();
					continue;
				}
				listing.next ??= read(reader);
				const result = await race([listing.next, wait().then(() => undefined)]);
				if (!result) continue;
				listing.next = undefined;
				// The source was replaced as its answer arrived: the new source's listing supersedes it.
				if (listing.source !== this.#source) continue;
				const { event, err } = result;
				if (err) {
					// Its readers fail, and a later reader asks again.
					stop();
					lister.abort(err);
				} else if (!event) {
					// A closing source ended its listing: the front moves to another source, or ends
					// with it. A live one's handler ended it, as a session does once the peer's
					// broadcast ended, so the front's listing ends too.
					if (listing.source?.from.closed.peek() !== undefined) {
						listing.reader = undefined;
					} else {
						stop();
						lister.end();
					}
				} else if (event.kind === "added") {
					if (!listing.stale.delete(event.name) && !listing.listed.has(event.name)) {
						listing.listed.add(event.name);
						lister.add(event.name, event.info);
					}
				} else if (event.kind === "removed" || event.kind === "finished") {
					unlist(listing.listed, listing.stale, event);
				} else {
					// What the new source still lacks at its SYNCED is gone.
					for (const name of listing.stale) {
						listing.listed.delete(name);
						lister.remove(name);
					}
					listing.stale.clear();
					lister.synced();
				}
			}
		} finally {
			stop();
			lister.close();
		}
	}

	/** Serve every track from `from` from now on, once each copy proves compatible. */
	serve(from: broadcast.Consumer): void {
		if (this.#concluded || this.#source?.from === from) return;
		const source = new Source(from);
		source.retain();
		this.#fromSession ||= source.fromSession;
		const previous = this.#source;
		this.#source = source;
		this.#retired();
		for (const route of this.#routes.values()) this.#splice(route, source);
		previous?.release();
	}

	/** Between routes: readers keep what they have and wait for the next one. */
	park(): void {
		if (this.#concluded || !this.#source) return;
		const previous = this.#source;
		this.#source = undefined;
		this.#retired();
		for (const route of this.#routes.values()) route.park();
		previous.release();
	}

	/** No route will replace the serving one: readers follow it to its end. Idempotent. */
	conclude(): void {
		if (this.#concluded) return;
		this.#concluded = true;
		for (const route of this.#routes.values()) route.conclude();
		this.#source?.release();
		this.#source = undefined;
		this.#retired();
	}

	// The source changed: wake what waits on it, and retire what was asked of the old one.
	#retired(): void {
		this.#changed.update((n) => n + 1);
		this.#retire.abort();
		this.#retire = new AbortController();
	}

	/**
	 * Whether a broadcast it served came from a session; see {@link BroadcastWire.fromSession}.
	 * Sticky, since the front can move back onto a session's route at any time.
	 */
	get fromSession(): boolean {
		return this.#fromSession;
	}

	// Splice `source`'s copy of a track in, once it claims the same content as the copies before.
	#splice(route: Route, source: Source): void {
		const recorded = this.#info.get(route.name);
		const previous = route.served;
		// The first copy of a track has nothing to match, and the source that said what it is
		// matches itself.
		if ((!recorded && !previous) || recorded?.source === source) {
			route.serve(source);
			return;
		}
		// The replaced copy keeps feeding the track while the new one answers. One that has not
		// answered yet is asked too, so a swap racing its first answer is still checked.
		source.retain();
		previous?.retain();
		// Abandoned once another route supersedes this one, releasing both.
		const retired = this.#retire.signal;
		const expected = recorded
			? Promise.resolve(recorded.info)
			: ask(previous as Source, route.name, retired).then(
					(info) => {
						this.#record(route.name, info, previous as Source);
						return info;
					},
					// Its route failing is no answer; a refusal is the track's.
					(err: unknown) => {
						if (routeFailed(err instanceof Error ? err : new Error(String(err)))) return undefined;
						throw err;
					},
				);
		Promise.all([expected, ask(source, route.name, retired)])
			.then(
				([expected, info]) => {
					if (this.#source !== source) return;
					// The copy it replaces never answered, so this one says what the track is.
					if (!expected) this.#record(route.name, info, source);
					// A copy claiming the same content with different metadata is refused rather
					// than reinterpreted.
					if (!expected || compatible(expected, info)) route.serve(source);
					else route.refuse(new Incompatible(route.name));
				},
				(err: unknown) => {
					if (this.#source !== source) return;
					const error = err instanceof Error ? err : new Error(String(err));
					// A route on its way out is no verdict: the next one is asked.
					if (!routeFailed(error)) route.refuse(error);
				},
			)
			.finally(() => {
				source.release();
				previous?.release();
			});
	}

	#route(name: string): Route {
		let route = this.#routes.get(name);
		if (route) return route;
		route = new Route(name);
		const created = route;
		created.idle = () => {
			if (this.#routes.get(name) === created) this.#routes.delete(name);
		};
		this.#routes.set(name, created);
		// A track whose properties another source already gave is checked like any replacement.
		if (this.#concluded) created.conclude();
		else if (this.#source) this.#splice(created, this.#source);
		return created;
	}

	// Check `source` claims the same properties for `name` as recorded, once per source: a fetch
	// never reads a copy a subscription would refuse.
	async #verify(name: string, source: Source): Promise<void> {
		const recorded = this.#info.get(name);
		if (recorded?.source === source) return;
		let verified = this.#verified.get(source);
		if (verified?.has(name)) return;
		const retired = this.#retire.signal;
		await this.#askServed(name, source, retired);
		const info = await ask(source, name, retired);
		// The first answer says what the track is; later sources have to match it.
		const expected = this.#info.get(name);
		if (!expected) this.#record(name, info, source);
		else if (!compatible(expected.info, info)) throw new Incompatible(name);
		verified ??= new Set();
		verified.add(name);
		this.#verified.set(source, verified);
	}

	// A copy a reader still reads says what the track is before `source` does: one logical track
	// has one set of properties. Its route failing is no answer; a refusal is the track's.
	async #askServed(name: string, source: Source, retired: AbortSignal): Promise<void> {
		const served = this.#routes.get(name)?.served;
		if (this.#info.has(name) || !served || served === source) return;
		let info: track.Info;
		try {
			info = await ask(served, name, retired);
		} catch (err) {
			if (routeFailed(err instanceof Error ? err : new Error(String(err)))) return;
			throw err;
		}
		this.#record(name, info, served);
	}

	#record(name: string, info: track.Info, source: Source): void {
		if (!this.#info.has(name)) this.#info.set(name, { info, source });
	}

	subscribe(name: string, options?: track.Subscription): track.Subscriber {
		if (this.consumer.closed.peek() !== undefined) throw new Error("broadcast is closed");
		const route = this.#route(name);
		return hooks.resumeSubscriber(
			name,
			options,
			(shell) => new Reader(route, shell, (info, source) => this.#record(name, info, source)),
		);
	}

	// Wait for the source to change, or `signal` to abort.
	#next(signal?: AbortSignal): Promise<void> {
		return until(Signal.race(this.#changed), signal);
	}

	async resolveTrackInfo(name: string, hold?: AbortSignal): Promise<track.Info> {
		for (;;) {
			const source = this.#source;
			if (!source) {
				if (this.#concluded) throw dropped();
				await this.#next(hold);
				continue;
			}
			source.retain();
			const retired = this.#retire.signal;
			let info: track.Info;
			try {
				await this.#askServed(name, source, retired);
				info = await ask(source, name, retired, hold);
			} catch (err) {
				// Another route took over before this one answered: ask it instead.
				if (retired.aborted && !hold?.aborted && !this.#concluded) continue;
				const error = err instanceof Error ? err : new Error(String(err));
				// The route failed: ask the next one, unless none will come.
				if (!routeFailed(error) || this.#concluded || hold?.aborted) throw error;
				if (this.#source === source) await this.#next(hold);
				continue;
			} finally {
				source.release();
			}
			const recorded = this.#info.get(name);
			// One logical track has one set of properties, whichever route answers.
			if (recorded && !compatible(recorded.info, info)) throw new Incompatible(name);
			this.#record(name, info, source);
			return recorded?.info ?? info;
		}
	}

	largest(name: string): track.Position | undefined {
		return this.#source?.wire.largest(name);
	}

	async fetchGroup(name: string, sequence: number, options?: track.FetchGroupOptions): Promise<GroupConsumer> {
		const fetched = await this.#fetch(name, sequence, options);
		let current = fetched.source;
		// Too old and evicted judge a subscription's delivery, not whether the group exists, so
		// neither is a verdict on a fetch: one answered from a group still arriving over a
		// subscription asks the route again for the rest when that delivery gives up on it.
		const undelivered = (failed: Error) =>
			routeFailed(failed) ||
			(failed instanceof StreamError && (failed.code === StreamCode.Old || failed.code === StreamCode.Evicted));
		// Carried across route changes like a subscribed group, but by fetching again: a fetch has
		// no copy of its own to find the rest in.
		hooks.recoverGroup(fetched.group, {
			changed: this.#changed,
			wants: (failed) => {
				if (failed && undelivered(failed)) return true;
				return this.#source !== undefined && this.#source !== current;
			},
			replace: async (_index, failed, signal) => {
				// A fetch that failed delivering may ask its own route again; a stalled one, or one
				// a verdict ended, only moves to another.
				const avoid = failed && undelivered(failed) ? undefined : current;
				const next = await this.#fetch(name, sequence, { ...options, signal }, avoid);
				// The fetch's hold on its route passes to the group on adoption, or goes with the discard.
				return {
					group: next.group,
					// The route applies the fetch's own budget.
					expiry: { expired: () => false, changed: [] },
					adopt: () => {
						current.release();
						current = next.source;
					},
					discard: () => {
						next.group.close();
						next.source.release();
					},
				};
			},
		});
		// Only once the group carries its recovery: reading `closed` before then would settle it
		// on a failure recovery could still continue, releasing the route while it is read.
		void fetched.group.closed.then(() => current.release());
		return fetched.group;
	}

	// Fetch a group from whichever route serves the track, asking the next one when a route
	// fails, and never `avoid`. The source comes back retained for as long as the group is read.
	async #fetch(
		name: string,
		sequence: number,
		options: track.FetchGroupOptions | undefined,
		avoid?: Source,
	): Promise<{ group: GroupConsumer; source: Source }> {
		for (;;) {
			const source = this.#source;
			if (!source || source === avoid) {
				if (this.#concluded) throw new NotFound(`group ${sequence}`);
				await this.#next(options?.signal);
				continue;
			}
			source.retain();
			// Abandoned once another route takes over, and asked of that one instead.
			const retired = this.#retire.signal;
			const signal = options?.signal ? AbortSignal.any([options.signal, retired]) : retired;
			try {
				await until(this.#verify(name, source), signal);
				const fetchGroup = source.wire.fetchGroup;
				if (!fetchGroup) throw new Error("fetch group is not supported by this session's version");
				const group = await fetchGroup(name, sequence, { ...options, signal });
				// A copy that already failed continues nothing: that is the route failing again.
				const failed = group.closed.peek();
				if (failed instanceof Error) {
					group.close();
					throw failed;
				}
				return { group, source };
			} catch (err) {
				source.release();
				if (retired.aborted && !options?.signal?.aborted && !this.#concluded) continue;
				const error = err instanceof Error ? err : new Error(String(err));
				// The route failed: ask the next one, unless none will come.
				if (!routeFailed(error) || this.#concluded || options?.signal?.aborted) throw error;
				if (this.#source === source) await this.#next(options?.signal);
			}
		}
	}
}

let makeFrontConsumer: (front: Front) => FrontConsumer;

/** A front's logical broadcast: a consumer handle whose tracks follow the front's route. */
class FrontConsumer extends broadcast.Consumer {
	#front: Front;

	private constructor(front: Front, state?: never) {
		super(state);
		this.#front = front;
		const view = wireOf(this);
		registerWire(this, {
			requested: view.requested,
			tracks: view.tracks,
			lister: view.lister,
			subscribe: (name, options) => front.subscribe(name, options),
			resolveTrackInfo: (name, hold) => front.resolveTrackInfo(name, hold),
			largest: (name) => front.largest(name),
			fetchGroup: (name, sequence, options) => front.fetchGroup(name, sequence, options),
			get fromSession() {
				return front.fromSession;
			},
		});
	}

	static {
		makeFrontConsumer = (front) => new FrontConsumer(front);
	}

	// Preserve the subclass, and the front behind it, when a handle is shared.
	override clone(): FrontConsumer {
		return new FrontConsumer(this.#front, this.shareState());
	}
}
