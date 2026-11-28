/**
 * Serve a front's logical track straight from its routes' copies. Mirrors `model::resume` in
 * `rs/moq-net`.
 *
 * A {@link Front} is one broadcast served through one route at a time: the origin re-sources it
 * when another route of the same identity takes over, since routes with one identity serve one
 * stream and so hold the same groups and frames. A reader of a logical track reads the
 * serving route's copy directly, through its own subscription: nothing is copied, and with one
 * route it is a passthrough.
 *
 * When the front switches, each reader subscribes to the new copy from the newest group it
 * handed out. A replaced copy keeps delivering until the new copy delivers its first group, then
 * is bounded just below it (or at the newest group handed out, if later), so a group only the old
 * route got after the switch is not lost. It is kept while it may still deliver a group under its
 * bound the reader lacks (unread, or still landing within the grace a passed group is waited for),
 * and while a group still read is read from it or may continue from it (it holds that group, or may
 * yet get it). Nothing under the bound is waited for past the reader's budget.
 *
 * A new copy is asked from where the reader got to, so it never has the groups below the reader's
 * local floor that the copies it replaced cached. Lowering that floor asks the newest route for
 * them through a backfill: another copy of that route, bounded to the groups the floor newly
 * covers, and kept like a replaced copy. A switch asks the same way for the groups below where the
 * reader got to that it still lacks, from the lowest one: one late, reordered, or in flight on the
 * replaced route still arrives within the reader's budget, as it would on one cursor.
 *
 * A group carries on across the switch through its {@link Recover}: once its copy stalls while a
 * newer route serves, it continues from the serving route's copy of the same group at the same
 * frame, found in that copy's cache or fetched from it. Once its copy fails, any other copy still
 * holding the group continues it first, since a group's name fixes its content.
 *
 * A group is handed out once however many copies deliver it: the reader skips any key its
 * subscriber already handed out, the same record that keeps a direct reader from taking a
 * re-created group twice.
 *
 * A reader asked for dispositions reports what the serving copy gave up, other than a route
 * failure, once no copy it replaced may still deliver it, and counts it as handed out so no copy
 * serves it later.
 *
 * The logical track ends cleanly once its serving copy does and no group the reader lacks may
 * still land below that end on a copy it replaced. Its `closed` settles, and the reader keeps each
 * copy only while it holds something left to read, as a direct reader keeps its buffer. It follows
 * the route only for the groups still read, so one whose route dies continues from the copy served
 * next, and leaves it once it holds nothing left to read. Its end is the first one any copy
 * declared, which a later mid-group end may refine; a copy declaring another breaks the track's
 * identity, which hurts only its own broadcast, so nothing checks that copies agree.
 *
 * @module
 */
import { Duration, Instant, now, sleepUntil } from "@moq/clock";
import { type Dispose, type Getter, race, Signal } from "@moq/signals";
import * as broadcast from "./broadcast.ts";
import { Delivered } from "./delivered.ts";
import { samePlaced } from "./demand.ts";
import { NotFound, Retry, retry, SessionError, StreamCode, StreamError, sessionClosed } from "./error.ts";
import type { Consumer as GroupConsumer } from "./group.ts";
import {
	type Ask,
	type Coverage,
	type Dispositions,
	type DropRun,
	type Expiry,
	type FoldTerms,
	finalKey,
	type GroupRecover,
	type Held,
	hooks,
	insertDrop,
	insertRun,
	type Land,
	type RecoverCause,
	type Recv,
	type Replacement,
	type ResumeShell,
	type ResumeSource,
	type Run,
	removeRun,
	runGaps,
	settles,
	staleHole,
	type Within,
} from "./internal.ts";
import { grace } from "./ledger.ts";
import { keyOf, type Lister, type Tracks, type TracksEvent } from "./listing.ts";
import * as track from "./track.ts";
import { max, min, UNBOUNDED } from "./util/key.ts";
import {
	AT_KEYS,
	asked,
	type Budget,
	EMPTY,
	isEmpty,
	type Joining,
	liveMillis,
	type Resolved,
	rangeTime,
	SINCE_ZERO,
	sameRanges,
	UNHELD,
	Window,
	widest,
} from "./window.ts";
import { type Broadcast as BroadcastWire, metaWireOf, registerWire, wireOf } from "./wire.ts";

/** A copy claiming the same content with different metadata: refused rather than reinterpreted. */
class Incompatible extends Error {
	constructor(name: string) {
		super(`track ${name} changed its properties across routes`);
		this.name = "Incompatible";
	}
}

/**
 * Whether `err` is the route failing to deliver rather than judging what it carried, so a new copy
 * may fix it. Mirrors the Rust `Error::retryable`.
 *
 * @internal
 */
export function routeFailed(err: Error): boolean {
	return retry(err) !== Retry.Never;
}

// Whether `err` from `source` judges its route rather than the track: a received route that lost
// the content refuses nothing, and the read waits for the next route. A local broadcast's word is
// the track's verdict. Mirrors the Rust front's `strands`.
function strands(err: Error, source: Source): boolean {
	return unroutable(err) && !source.local;
}

// Whether `err` says the route is going away rather than that it gave up on the copy: its
// answer is no verdict, and the next route is asked. Mirrors the Rust front's `closing`.
function closing(err: Error): boolean {
	if (err instanceof StreamError) return err.code === StreamCode.GoingAway || err.code === StreamCode.SessionClosed;
	return err instanceof SessionError || sessionClosed(err);
}

// A source losing its route to the content: only another route continues it. Mirrors the Rust
// `Error::unroutable`.
function unroutable(err: Error): boolean {
	return retry(err) === Retry.Reroute;
}

// A copy's delivery failure says nothing about its content. A terminal track verdict does.
// Mirrors the Rust `copy_failed`.
function copyFailed(copy: Copy): boolean {
	const error = copy.sub.closed.peek();
	return error instanceof Error && routeFailed(error);
}

// Whether the copy's route failed, even after its declared end settled, which ends its readers
// cleanly: a group it closed short may hold more on the route replacing it. Only a route going
// away counts then: a subscription reset alone leaves the route serving, and nothing replaces
// it. Mirrors the Rust `route_lost`, whose copies abort after their end only with the session.
function routeLost(copy: Copy): boolean {
	const aborted = hooks.trackAborted(copy.sub);
	return copyFailed(copy) || (aborted !== undefined && closing(aborted));
}

// Every `track.Info` field but `unchecked`, so one added later fails to compile until it is
// listed here. `unchecked` says how a copy arrived, not what it holds, so copies over different
// lite versions still match. `ietf` is compared, like Rust's moq-transport `properties`.
const INFO_FIELDS = Object.keys({
	compression: true,
	timescale: true,
	maxAge: true,
	priority: true,
	refresh: true,
	ietf: true,
} satisfies Record<Exclude<keyof track.Info, "unchecked">, true>) as (keyof track.Info)[];

/** Two copies claiming the same content must agree on all of it, like Rust's `Info` equality. */
function compatible(a: track.Info, b: track.Info): boolean {
	return INFO_FIELDS.every((field) => {
		const [l, r] = [a[field], b[field]];
		if (field === "refresh") return sameRefresh(a.refresh, b.refresh);
		return l instanceof Duration && r instanceof Duration ? l.eq(r) : l === r;
	});
}

function sameRefresh(a: track.Refresh, b: track.Refresh): boolean {
	const interval = a.interval === null || b.interval === null ? a.interval === b.interval : a.interval.eq(b.interval);
	return a.ratio === b.ratio && interval;
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

let sources = 0;

/** One route's broadcast, held while the front serves it or a reader still reads a copy of it. */
class Source {
	/**
	 * Names this source where it outlives its route, as a front's record of who said what a
	 * track is: a reference there would keep a retired route's session reachable.
	 */
	readonly id = sources++;
	readonly wire: BroadcastWire;
	/** What a metadata reader reads of it: its metadata copies where it keeps them apart, else `wire`. */
	readonly meta: BroadcastWire;
	/** The table's handle this was served from, so serving it again is a no-op. */
	readonly from: broadcast.Consumer;
	/** The local broadcast rather than a received route: its word on a track is the verdict. */
	readonly local: boolean;
	#consumer: broadcast.Consumer;
	#holds = 0;

	constructor(from: broadcast.Consumer, local = false) {
		this.from = from;
		this.local = local;
		this.#consumer = from.clone();
		this.wire = wireOf(this.#consumer);
		this.meta = metaWireOf(this.#consumer);
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
	/** Whether it serves the track's metadata copy, which each source keeps apart or not. */
	readonly meta: boolean;
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
	/**
	 * The source serving before the replacements that have yet to deliver, held until a copy of
	 * `source` delivers: a reader arriving meanwhile reads it too, as the readers before it do, so
	 * a replacement failing first leaves it serving. The Rust front only swaps once one delivers.
	 */
	incumbent?: Source;
	/** The source a read idles on, having lost the route; cleared once asked again or replaced. */
	#stranded?: Source;
	/**
	 * The source the front is splicing in while its copy's info is in flight: a short close on the
	 * serving copy waits for it rather than standing. Mirrors the Rust route's `staged`.
	 */
	staged?: Source;
	/** Bumped on every change above, waking parked reads. */
	readonly changed = new Signal(0);
	readonly readers = new Set<Reader>();
	/** Called once the last reader leaves. */
	idle: () => void = () => {};
	/** Called once a reader subscribes a copy, or moves one onto a new source. */
	copied: () => void = () => {};

	constructor(name: string, meta: boolean) {
		this.name = name;
		this.meta = meta;
	}

	/** The copy of this track `source` serves. */
	view(source: Source): BroadcastWire {
		return this.meta ? source.meta : source.wire;
	}

	#bump(): void {
		this.changed.update((n) => n + 1);
	}

	/** Serve the track from `source` from now on, subscribing every reader to it. */
	serve(source: Source): void {
		// A refusal ends the track: no later source continues it, compatible or not.
		if (this.concluded || this.refusal || this.source === source) return;
		this.#stranded = undefined;
		this.generation++;
		// An undelivered source hands over the incumbent it held, not itself.
		if (!this.incumbent) this.#hold(this.source);
		this.source = source;
		this.served = source;
		this.#bump();
		for (const reader of [...this.readers]) reader.sync();
	}

	/**
	 * Serve `to` again in place of `from`, a replacement whose copy failed before delivering, even
	 * past a refusal or a conclusion: `to` served before it, and concluding only stops a future
	 * source taking over. Returns whether `to` serves again.
	 */
	revert(from: Source, to: Source): boolean {
		// A concluded route serves nothing new, but the failed replacement was still its last.
		if (this.source !== from && !(this.concluded && this.source === undefined)) return false;
		this.generation++;
		this.#hold(undefined);
		this.source = to;
		this.served = to;
		this.#bump();
		for (const reader of [...this.readers]) reader.sync();
		return true;
	}

	/**
	 * Between routes: readers wait for the next one, keeping what they have. The source that served
	 * stays the incumbent until a later one delivers, as a replacement's would.
	 */
	park(): void {
		if (!this.source) {
			if (this.staged) this.unstage(this.staged);
			return;
		}
		this.staged = undefined;
		this.generation++;
		if (!this.incumbent) this.#hold(this.source);
		this.source = undefined;
		this.#bump();
	}

	/** The front asks `source` to take over, until {@link unstage} settles it. */
	stage(source: Source): void {
		this.staged = source;
		this.#bump();
	}

	/** The front settled whether `source` takes over, unless a later stage replaced it. */
	unstage(source: Source): void {
		if (this.staged !== source) return;
		this.staged = undefined;
		this.#bump();
	}

	/** A read idles on `source`, which lost the route, until the table moves: see {@link Front.moved}. */
	strand(source: Source): void {
		this.#stranded = source;
	}

	/** Whether a read idles on a lost route, which the table moving now ends. */
	unstrand(): boolean {
		if (!this.#stranded) return false;
		this.#stranded = undefined;
		return true;
	}

	/** Subscribe every reader afresh to the source that still serves, once its copy failed. */
	resubscribe(): void {
		if (!this.source) return;
		this.generation++;
		this.#bump();
		for (const reader of [...this.readers]) reader.sync();
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
		this.staged = undefined;
		this.#hold(undefined);
		this.source = undefined;
		this.#bump();
	}

	/** A copy of `source` delivered: it no longer needs the source it replaced. */
	delivered(source: Source): void {
		if (source === this.source) this.#hold(undefined);
	}

	#hold(incumbent: Source | undefined): void {
		incumbent?.retain();
		this.incumbent?.release();
		this.incumbent = incumbent;
	}

	leave(reader: Reader): void {
		if (!this.readers.delete(reader) || this.readers.size > 0) return;
		// Nobody reads the track, so nothing is left to protect from the replacement.
		this.#hold(undefined);
		this.idle();
	}
}

/** The reader's ranges, and the live joins placed among them, resolved where its copies placed them. */
interface Resolution {
	from: readonly track.Range[];
	placed?: ReadonlyMap<number, bigint>;
	window: Window;
}

/** A route's copy, as one reader reads it. */
interface Copy {
	generation: number;
	source: Source;
	sub: track.Subscriber;
	/** Where the subscription was asked to start, so demand updates never ask for less. */
	floor?: bigint;
	/**
	 * A backfill asked from at or below a range's time start under the serving copy's resume point:
	 * its floor is where it was asked from, not a resume point of its own, so the group holding that
	 * time, which may lie under it, is read. A backfill of gaps alone owes nothing under its floor.
	 */
	backfill?: true;
	/** The reader's ranges {@link Copy.resolved} are for. */
	from?: readonly track.Range[];
	/**
	 * Each of those ranges resolved, by index, so clamping never widens one: as the serving copy
	 * had them when this one was subscribed, until this one resolves them itself.
	 */
	resolved?: (Resolved | undefined)[];
	/** Replaced by a newer route. */
	replaced: boolean;
	/**
	 * Bounded once the newer copy delivered its first group: nothing past this group is taken
	 * from it. `null` when that leaves nothing. Unset while it is the only copy still feeding
	 * groups the newer one may never have.
	 */
	until?: bigint | null;
	/**
	 * The group channel ran out: failed, or cleanly with how many groups had arrived on the copy
	 * by then, since one arriving later is still in flight below its end, which a read still open
	 * takes.
	 */
	done?: Error | number;
	/**
	 * When a newer group superseded every group under the bound: the newer copy's first group, or
	 * the track's end. What the reader still lacks from it is waited for from here, for no longer
	 * than the reader's budget.
	 */
	since?: Instant;
	/** Groups still read that came from this copy, which keep a replaced copy subscribed. */
	lease: number;
	/**
	 * It handed out a group, or got one it holds back for a placement, so a failure is its route
	 * failing mid-serve rather than refusing.
	 */
	delivered?: boolean;
	/**
	 * One past the highest group the reader's window dropped from it, which it no longer offers
	 * though a widened window may name it again: a resume point, as {@link Copy.floor} is.
	 *
	 * The Rust reader has no counterpart, since it never reaches this shape. Its front keeps a track
	 * being read on its source until that copy ends, so a copy it replaced has stopped placing the
	 * window by the time the next one subscribes. That copy subscribes with the window's resolution,
	 * so its own ranges hold back what the window leaves out, still cached for a widening to offer.
	 * This front moves a track being read as soon as a better route of its identity answers. A copy
	 * it replaced, still delivering, can then place the window after the serving copy subscribed with
	 * nothing resolved, and that copy offers a group the window leaves out: the reader takes it.
	 */
	dropped?: bigint;
	/**
	 * The groups {@link Reader.backfillGaps} left it to drain that the reader still lacks, each one
	 * it held then: once one no longer drains here, or the copy is let go, the serving route is asked
	 * for it after all.
	 */
	drains?: Set<bigint>;
}

/**
 * Subscribe to a route's copy, or to a failed stand-in when that route can no longer serve. A
 * copy parks past the end of a bounded window: the logical reader judges that end across its
 * copies; see {@link Reader.exhausted}.
 */
function subscribe(wire: BroadcastWire, name: string, subscription: track.Subscription): track.Subscriber {
	try {
		return hooks.parked(wire.subscribe(name, subscription));
	} catch (err) {
		const failed = new track.Producer(name);
		failed.abort(err instanceof Error ? err : new Error(String(err)));
		return hooks.parked(failed.subscribe(subscription));
	}
}

/**
 * Take a copy's next group, in sequence or arrival order. With `dispositions`, a run the copy gives
 * up on the way comes back first; see {@link Dispositions}.
 */
function take(copy: Copy, ordered: boolean, floor?: bigint, dispositions: Dispositions = false, admit?: bigint): Recv {
	return ordered ? hooks.tryNextGroup(copy.sub, floor, admit) : hooks.tryRecvGroup(copy.sub, dispositions);
}

/** Where `copy` stopped offering the groups below, which only a backfill of its route reads. */
function resumePoint(copy: Pick<Copy, "floor" | "dropped">): bigint | undefined {
	return copy.dropped === undefined ? copy.floor : max(copy.floor ?? 0n, copy.dropped);
}

/**
 * The time start of `range` that a copy asked from `floor` raised to it, if any: that copy's
 * resolution says nothing of which group holds it, which only a copy asked from below it places.
 * Mirrors the Rust `raised`.
 */
function raised(range: track.Range | undefined, floor: bigint | undefined): bigint | undefined {
	const start = range && rangeTime(range);
	return floor !== undefined && start !== undefined && start < floor ? start : undefined;
}

/** The first group past where `copy` was replaced: 0 for one replaced before any group. */
function copyEnd(copy: Pick<Copy, "until">): bigint | undefined {
	if (copy.until === undefined) return undefined;
	return copy.until === null ? 0n : copy.until + 1n;
}

/** One reader's subscription to a logical track; see the module docs. */
class Reader implements ResumeSource {
	readonly route: Route;
	#shell: ResumeShell;
	/** The route generation last read. */
	#generation?: number;
	/** The reader's preferences as last mirrored into its copies; see {@link #follow}. */
	#mirrored?: track.Subscription;
	/**
	 * Oldest first, except that a backfill goes before the copy of the route it backfills: the last
	 * one is the serving route's own, when it is subscribed.
	 */
	#copies: Copy[] = [];
	/** The reader's ranges, and the {@link #window} they resolved to. */
	#resolution?: Resolution;
	/** The reader's cursor the copies were last bounded at: a lower start asks for a backfill. */
	#bounded: { start: bigint; end?: bigint };
	/** The newest group handed out: where a new copy is asked to start. */
	#newest?: bigint;
	/**
	 * The skipped run from the start of each key run the cursor keeps out of a gap under the serving
	 * copy's resume point, recorded as the copies declare it, so it outlives the copy and the cache
	 * that held it. Only that run, at most two a gap however many routes come and go: the rest is
	 * asked of the serving route again; see {@link #asking}.
	 */
	#skipped = new Delivered();
	/**
	 * Set once the route changed, so the serving copy's resume point may pass over groups the
	 * reader lacks. Kept until the gaps are backfilled; see {@link #backfillGaps}.
	 */
	#resumed = false;
	/**
	 * The groups a backfill of route `generation` failed to get, which that route is not asked
	 * for again until the route or the reader's preferences change: a backfill it refuses would
	 * only fail again, and asking each read would spin. Mirrors the Rust `Reader::refused`.
	 */
	#refused?: { generation: number; start: bigint; end: bigint };
	/** Bumped when this reader subscribes a new copy or a copy's last lease ends, waking its parked reads. */
	#changed = new Signal(0);
	#dispose: Dispose[] = [];
	#closed = false;
	/**
	 * Set once the logical track ended cleanly: the reader drains what its copies hold and follows
	 * the route only for the groups still read; see {@link #rerouted}. Holds the serving copy's
	 * largest then, kept once that copy goes.
	 */
	#ended?: { largest?: track.Position; object?: track.Position; ended: true };
	/**
	 * Copies of the routes served after the track ended, newest last, subscribed only while a group
	 * handed out is still read: they continue it off a route that dies, and hand out nothing.
	 * Mirrors the Rust `Phase::Finished { routed }`.
	 */
	#rerouted: Copy[] = [];
	/**
	 * The first end any copy declared: the logical track's, whichever copy serves. A copy declaring
	 * another breaks the track's identity, which hurts only its own broadcast, so nothing checks that
	 * copies agree.
	 */
	#declared?: track.Position;
	// The error a copy's end carried, kept with the boundary so releasing that copy keeps it.
	#declaredError?: Error;
	/** The subscriber's record of groups handed out, as the last read passed it. */
	#delivered = new Delivered();
	/** Runs the serving copy gave up and the reader has yet to report; see {@link #tryDrop}. */
	#drops: DropRun[] = [];
	/** A copy that declared the track's end ran out cleanly at it, even if since let go. */
	#settled = false;
	/** Whether groups were ever asked for; until then the reader keeps no replaced copy. */
	#groupsRead = false;
	/** The groups handed out and still read, which other copies may back; see {@link #backs}. */
	readonly out = new Set<Recover>();

	/**
	 * What a group moved onto another copy is judged within: this reader's window below its cursor
	 * cap, not the copy's own subscription, which starts at the switch's boundary. Mirrors the Rust
	 * `Recover::window`.
	 */
	readonly within: Within;
	/** Every group handed out and still open, which ends with the reader. */
	#open = new Set<GroupConsumer>();
	/** The soonest deadline a read waits on, armed to wake it then. */
	#wall?: { at: number; timer: AbortController };
	/** Records the properties a copy committed, and which source it came from. */
	#record: (info: track.Info, source: Source) => void;
	/** The first frame of each group the reader reads whole from, applied to every copy; see `hooks.wholeGroups`. */
	#whole?: (key: bigint) => number | undefined;

	/** `parent` is the newest copy of the reader this one forks, if any. */
	constructor(route: Route, shell: ResumeShell, record: (info: track.Info, source: Source) => void, parent?: Copy) {
		this.route = route;
		this.#shell = shell;
		this.within = {
			contains: (key) => this.#inWindow(key),
			budget: () => this.#readerBudget(),
			end: () => shell.cursor.peek().end,
			changed: [shell.cursor, shell.subscription],
		};
		this.#record = record;
		this.#mirrored = shell.subscription.peek();
		this.#bounded = shell.cursor.peek();
		route.readers.add(this);
		this.#dispose.push(
			shell.subscription.subscribe(() => this.#follow()),
			shell.cursor.subscribe(() => this.#follow()),
		);
		// A fork starts on its parent's newest copy, at that copy's generation: the copy keeps its
		// retained groups even where no route serves the track, between routes or once the front
		// concluded.
		if (parent) {
			this.#generation = parent.generation;
			const asked: Pick<Copy, "from" | "resolved"> = {};
			const copy: Copy = {
				generation: parent.generation,
				source: parent.source,
				sub: parent.sub.fork(this.#wire(asked)),
				...asked,
				replaced: false,
				lease: 0,
			};
			this.#add(copy);
			this.#joinIncumbent(copy);
		}
		// Subscribed to the serving copy at once, so groups it has now are the reader's even if
		// the route changes before the first read, and to each copy after.
		this.sync();
	}

	/**
	 * Follow the route: apply the reader's preferences and cursor to its copies, subscribe to a
	 * new copy, and mark the ones it replaced.
	 */
	sync(): void {
		if (this.#closed) return;
		this.#follow();
		if (this.#ended) {
			this.#reroute();
			return;
		}
		const route = this.route;
		if (this.#generation === route.generation) return;
		this.#generation = route.generation;
		// What a former route gave up is no verdict on the one that serves now.
		this.#drops = [];
		const source = route.source;
		if (!source) return;
		// The new copy starts at the boundary, so a lower group no replaced copy holds is asked of
		// its route once a read says which; see #backfillGaps.
		this.#resumed = true;
		// One subscription split at one boundary: the largest group any copy holds, never below one
		// handed out. The new copy starts there, and each copy it replaces is capped there now, so
		// it finishes the groups up to it and sends nothing newer.
		const boundary = this.#boundary();
		// Cloned handles share closed, so a returning route reuses its subscription and
		// leases rather than replaying its cache and retaining another copy per flap. The newest
		// is the route's own copy, not a backfill of it.
		// A copy that failed is never reused: the route subscribes afresh after one.
		const index = this.#copies.findLastIndex(
			(copy) => copy.source.from.closed === source.from.closed && !(copy.sub.closed.peek() instanceof Error),
		);
		const [returning] = index === -1 ? [] : this.#copies.splice(index, 1);
		if (boundary !== undefined) {
			for (const copy of this.#copies) {
				copy.replaced = true;
				this.#cap(copy, boundary);
			}
		} else {
			// Nothing held nor handed out: nothing to split, so the new copy serves it all. Kept and
			// asked for nothing, a copy's subscription would end with the track, and a fetch then
			// landing a later group would read as the whole of it. Only the incumbent's copy stays,
			// uncapped until the new copy delivers (see #observe), as the Rust front splices a
			// replacement only then; the rest go, as the Rust `Reader::sync` drops them.
			const incumbent = route.incumbent;
			this.#dropCopies((copy) => copy.source.from.closed !== incumbent?.from.closed);
			for (const copy of this.#copies) copy.replaced = true;
		}
		if (returning) {
			const copy = returning;
			source.retain();
			copy.source.release();
			copy.source = source;
			copy.generation = route.generation;
			copy.replaced = false;
			copy.until = undefined;
			copy.since = undefined;
			copy.done = undefined;
			copy.drains = undefined;
			// Resume where a fresh copy would: a group it buffered while replaced, below the
			// boundary, is the capped copies' to deliver.
			if (boundary !== undefined) copy.floor = max(copy.floor ?? 0n, boundary);
			copy.backfill = undefined;
			this.#update(copy);
			this.#bound(copy);
			this.#copies.push(copy);
			this.wake();
			route.copied();
			return;
		}
		const asked: Pick<Copy, "floor" | "from" | "resolved"> = { floor: boundary };
		const serving: Copy = {
			generation: route.generation,
			source,
			// The wire request carries the floor from the first message: some sessions read the
			// demand only once.
			sub: subscribe(route.view(source), route.name, this.#wire(asked)),
			...asked,
			replaced: false,
			lease: 0,
		};
		this.#add(serving);
		this.#joinIncumbent(serving);
	}

	/**
	 * Arriving before the serving copy delivered, read the source it replaced too, as the readers
	 * before this one still do.
	 */
	#joinIncumbent(serving: Copy): void {
		const route = this.route;
		const incumbent = route.incumbent;
		if (!incumbent || serving.source !== route.source) return;
		if (hooks.latestKey(serving.sub) !== undefined) {
			this.#delivering(serving);
			return;
		}
		if (this.#copies.some((copy) => copy.source.from.closed === incumbent.from.closed)) return;
		const asked: Pick<Copy, "floor" | "from" | "resolved"> = { floor: serving.floor };
		this.#add(
			{
				generation: serving.generation - 1,
				source: incumbent,
				sub: subscribe(route.view(incumbent), route.name, this.#wire(asked)),
				...asked,
				replaced: true,
				lease: 0,
			},
			this.#copies.indexOf(serving),
		);
	}

	/**
	 * Once the track ended, subscribe the serving route's copy for the groups still read, from the
	 * lowest, should the route have moved past the copies the reader holds: their recoveries
	 * continue there. Nothing is handed out from it, so the reader stays on the copy the track
	 * ended on.
	 */
	#reroute(): void {
		const route = this.route;
		const source = route.source;
		if (source && this.out.size > 0 && !this.copy(route.generation)) {
			let floor: bigint | undefined;
			for (const recover of this.out) floor = min(floor ?? recover.key, recover.key);
			const asked: Pick<Copy, "floor" | "from" | "resolved"> = { floor };
			const copy: Copy = {
				generation: route.generation,
				source,
				sub: subscribe(route.view(source), route.name, this.#wire(asked)),
				...asked,
				replaced: false,
				lease: 0,
			};
			source.retain();
			this.#bound(copy);
			this.#rerouted.push(copy);
		}
		this.#unroute();
	}

	/**
	 * Let go of the rerouted copies no group still read needs: each one a newer route superseded,
	 * and every one once none is read. Once the ended reader holds nothing left to hand out either,
	 * it leaves the route.
	 */
	#unroute(): void {
		if (!this.#ended) return;
		const newest = this.out.size > 0 ? this.#rerouted.at(-1) : undefined;
		this.#rerouted = this.#rerouted.filter((copy) => {
			if (copy === newest || copy.lease > 0) return true;
			this.#drop(copy);
			return false;
		});
		if (this.out.size === 0 && this.#copies.length === 0) this.route.leave(this);
	}

	/**
	 * Where a switch splits the subscription: the largest group any copy holds, never below the
	 * newest handed out. `undefined` while neither exists, when the new copy is asked for the ranges
	 * as the copies it replaces were, and they go. Mirrors the Rust `Reader::boundary`.
	 */
	#boundary(): bigint | undefined {
		let boundary = this.#newest;
		for (const copy of this.#copies) {
			const latest = hooks.latestKey(copy.sub);
			if (latest !== undefined && (boundary === undefined || latest > boundary)) boundary = latest;
		}
		return boundary;
	}

	/**
	 * Take nothing past group `until` from replaced `copy`, and ask for nothing past it on the wire:
	 * the copy replacing it starts there. A cap only narrows. Mirrors the Rust `Copy::cap`, by group
	 * only: a JS switch resumes no group partway, so it has no frame to stop the old copy at.
	 */
	#cap(copy: Copy, until: bigint): void {
		if (copy.until === null || (copy.until !== undefined && copy.until <= until)) return;
		copy.until = until;
		this.#update(copy);
		this.#bound(copy);
	}

	/** Read `copy` as the newest one, or at `index`, recording the properties it commits. */
	#add(copy: Copy, index = this.#copies.length): void {
		const { source } = copy;
		this.#stamp(copy);
		source.retain();
		this.#readWhole(copy);
		this.#bound(copy);
		this.#copies.splice(index, 0, copy);
		this.route.copied();
		const shell = this.#shell;
		const record = this.#record;
		copy.sub.info().then(
			(info) => {
				record(info, source);
				shell.commit(info);
			},
			() => {
				// A terminal refusal before metadata is the subscription's answer too. An info
				// reader never polls the group cursor that would otherwise propagate it.
				if (this.#shell.info() === undefined) this.#end((copy) => copy.sub.closed.peek());
			},
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

	/**
	 * What `copy` is asked for: the reader's own preferences, from its floor and through its
	 * replacement. Mirrors the Rust `Copy::wire`.
	 */
	#wire(copy: Pick<Copy, "floor" | "until" | "from" | "resolved"> & { sub?: track.Subscriber }): track.Subscription {
		const preferences = this.#shell.subscription.peek() ?? track.Subscription.default();
		const ranges = preferences.ranges ?? [{}];
		if (copy.sub && copy.from !== undefined && sameRanges(copy.from, ranges)) {
			// A range keeps its first resolution: the copy re-resolves its clamped wire, so its own
			// resolution of a capped or floored request would move the endpoints for good.
			const own = hooks.servingResolved(copy.sub);
			const kept = copy.resolved ?? [];
			copy.resolved = ranges.map((_, index) => kept[index] ?? own[index]);
		} else if (copy.sub) {
			// Replaced ranges resolve against its cache now, as any subscriber's do.
			copy.from = ranges;
			copy.resolved = hooks.resolveRanges(copy.sub, { ...preferences, ranges });
		} else if (copy.from === undefined || !sameRanges(copy.from, ranges)) {
			// A new copy starts from where the reader's ranges resolved.
			copy.from = ranges;
			copy.resolved = this.#window().resolved();
		}
		// A live join placed already, here or where the reader's own request came from, is asked
		// placed, so the copy starts it there and still expires it under its max.
		const wire = asked(ranges, copy.floor, copyEnd(copy), copy.resolved ?? [], preferences.placed);
		const { placed: _, ...rest } = preferences;
		return wire.placed.size > 0
			? { ...rest, ranges: wire.ranges, placed: wire.placed }
			: { ...rest, ranges: wire.ranges };
	}

	/**
	 * The reader's ranges resolved, a time start or live join where the first copy to place it
	 * placed it, whatever its failover cap: that bounds what the copy delivers, not where the ranges
	 * resolve. Kept once placed, past that copy being let go. A range stays as first held, unplaced,
	 * while its start waits on a copy to place it. A copy resolves a time start against the groups
	 * it holds, which may place it below where the reader's window did: the window decides what is
	 * handed out. Mirrors the Rust `Reader::window`.
	 */
	#window(): Window {
		const ranges = this.#ranges();
		const placed = this.#shell.subscription.peek()?.placed;
		let resolution = this.#resolution;
		if (
			!resolution ||
			(resolution.from !== ranges && !sameRanges(resolution.from, ranges)) ||
			!samePlaced(resolution.placed, placed)
		) {
			resolution = this.#fresh(ranges);
			this.#resolution = resolution;
		}
		if (resolution.window.settled) return resolution.window;
		for (const copy of this.#copies) {
			if (resolution.window.settled) break;
			// A copy whose delivery failed places a time start past its last group as if nothing
			// followed, but the front's next copy continues the track: only that copy knows which
			// group holds it, until the track concludes.
			if (!this.route.concluded && copyFailed(copy)) continue;
			if (!copy.from || !sameRanges(copy.from, ranges)) continue;
			// A closed copy's subscriber is no longer updated, so its own resolution is of the ranges
			// it had then; the one it took when asked for these answers instead.
			const own = copy.sub.closed.peek() === undefined ? hooks.servingResolved(copy.sub) : (copy.resolved ?? []);
			const cap = copyEnd(copy);
			// A failover cap at or below where a range starts empties the copy's range, which then
			// says nothing of where the reader's own range starts. One at or below where it ends cut
			// that end short, so it says nothing of where the reader's range ends.
			const resolved = own.map((at, index) => {
				// A copy asked from above a range's time start says nothing of where it starts: a
				// backfill asked from below places it; see #backfillGaps.
				if (!at || raised(ranges[index], copy.floor) !== undefined) return undefined;
				if (cap !== undefined && at.end !== undefined && at.end <= at.start) return undefined;
				return cap === undefined || (at.end !== undefined && at.end < cap) ? at : { ...at, end: undefined };
			});
			this.#resolve(resolved);
		}
		return resolution.window;
	}

	/**
	 * A resolution of the reader's `ranges` with nothing placed but a live join already placed where
	 * its preferences say, as each copy is asked to place it. Mirrors the Rust `mirrored_window`.
	 */
	#fresh(ranges: readonly track.Range[]): Resolution {
		const placed = this.#shell.subscription.peek()?.placed;
		const joining = (key: bigint): Joining => {
			const at = placed?.get(Number(key));
			return at === undefined ? SINCE_ZERO : { at };
		};
		const keyed = ranges.map((range, index) => [BigInt(index), range] as const);
		return { from: ranges, placed, window: Window.keyed(keyed, undefined, UNHELD, joining) };
	}

	/**
	 * The time starts of the unsettled ranges that the serving copy's resume point raised, by key:
	 * no copy asked from there places them, only a backfill from below. Mirrors the Rust
	 * `Reader::raised`.
	 */
	#raised(): Map<bigint, bigint> {
		const raisedAt = new Map<bigint, bigint>();
		const floor = this.#copies.at(-1)?.floor;
		const window = this.#resolution?.window;
		if (floor === undefined || !window || window.settled) return raisedAt;
		const ranges = this.#ranges();
		for (const key of window.unsettled()) {
			const time = raised(ranges[Number(key)], floor);
			if (time !== undefined) raisedAt.set(key, time);
		}
		return raisedAt;
	}

	/**
	 * Place each unsettled {@link #window} range where a copy resolved it, on its own: one whose
	 * start still waits leaves the rest placed, so a publisher names their starts. Mirrors the Rust
	 * `Reader::resolve`.
	 */
	#resolve(resolved: readonly (Resolved | undefined)[]): void {
		const resolution = this.#resolution;
		if (!resolution) return;
		const placed: [bigint, track.Range][] = [];
		for (const key of resolution.window.unsettled()) {
			const range = resolution.from[Number(key)];
			if (range === undefined) continue;
			const at = resolved[Number(key)];
			if (!at) {
				// A range the copy left unresolved keeps its time start or live join, which only a copy
				// can place.
				const unplaced = (rangeTime(range) ?? 0n) > 0n || liveMillis(range) !== undefined;
				// Placed as it is: nothing of it waits on a copy.
				if (!unplaced || isEmpty(range)) placed.push([key, range]);
				continue;
			}
			if (range.end !== undefined && at.end === 0n) {
				placed.push([key, EMPTY]);
				continue;
			}
			const out: track.Range = {};
			// One that reached back to the oldest group still does, as on a plain reader; a live join
			// starts where the copy placed it.
			if (at.floored) out.start = at.start;
			if (range.end !== undefined) out.end = at.end === undefined ? range.end : at.end - 1n;
			placed.push([key, out]);
		}
		if (placed.length > 0) resolution.window.edit([], placed, undefined, AT_KEYS);
	}

	/** The reader's ranges, as its preferences name them. */
	#ranges(): readonly track.Range[] {
		return (this.#shell.subscription.peek() ?? track.Subscription.default()).ranges ?? [{}];
	}

	/** Whether the reader's window names `key`; see {@link #window}. */
	#inWindow(key: bigint): boolean {
		return this.#window().contains(key);
	}

	/**
	 * Whether the reader's window and cursor name a group from `floor` up to `key` that `delivered`
	 * lacks and that may yet land on `copy`: one outside the cursor never lands on a copy, nor one
	 * `copy` declares skipped, nor any on a failed copy, which receives nothing more. Mirrors the
	 * Rust `Reader::owes_below`.
	 */
	#owesBelow(copy: Copy, floor: bigint, key: bigint, delivered: Delivered): boolean {
		if (copy.sub.closed.peek() instanceof Error) return false;
		const cursor = this.#shell.cursor.peek();
		return this.#window()
			.runs(max(floor, cursor.start), min(key, cursor.end ?? key))
			.some((run) =>
				[...delivered.gaps(run.start, run.end)].some((gap) => {
					for (let at = gap.start; at < gap.end; ) {
						const past = hooks.holeEnd(copy.sub, at, false);
						if (past === undefined || past <= at) return true;
						at = past;
					}
					return false;
				}),
			);
	}

	#update(copy: Copy): void {
		// A closed copy still resolves replaced ranges against the groups it holds, for the window.
		const wire = this.#wire(copy);
		if (copy.sub.closed.peek() === undefined) {
			// Restated, as the reader's ranges were, unless they only retuned a live join's max: a join
			// whose max alone changed is then placed afresh rather than retuned where the copy placed
			// the old one. Mirrors the Rust `Copy::ask_whole`.
			if (this.#shell.retuned()) copy.sub.update(wire);
			else hooks.restateSubscriber(copy.sub, wire);
			this.#stamp(copy);
		} else {
			// Its ranges stay as resolved when it closed, but the groups it still holds are picked in
			// the reader's Group Order. Mirrors the Rust `Copy::send`, which edits every copy.
			const { priority = 0, order = "descending" } = wire;
			hooks.editSubscriber(copy.sub, { retire: [], add: [], priority, order });
		}
	}

	/**
	 * Note each live join `copy` is asked for as the reader added it, so an answer the copy recorded
	 * since places it, even one recorded before this subscription. The copy keys its ranges by
	 * position, as the reader's are ordered. Mirrors the Rust `Copy::stamp`.
	 */
	#stamp(copy: Copy): void {
		const since = this.#shell.since().map((tick, index) => [BigInt(index), tick] as const);
		hooks.stampSubscriber(copy.sub, since);
	}

	/**
	 * Apply the reader's preferences and cursor to every copy once they changed. Their signals
	 * notify a microtask after a change, so every read applies them first too: a copy then picks
	 * and judges its groups by what the reader holds now, such as a just-lowered live {@link track.Live.max} or
	 * replaced ranges, not its last ones. Mirrors the Rust `Reader::sync`.
	 */
	#follow(): void {
		const subscription = this.#shell.subscription.peek();
		if (subscription !== this.#mirrored) {
			this.#mirrored = subscription;
			// New preferences may ask what a backfill failed to get.
			this.#refused = undefined;
			for (const copy of [...this.#copies, ...this.#rerouted]) this.#update(copy);
			// A copy just resolved replaced ranges against its cache, before its failover cap raised
			// them, so the reader takes that resolution now, the newest copy's first.
			const ranges = this.#ranges();
			if (this.#resolution && !sameRanges(this.#resolution.from, ranges)) {
				// The serving copy's resume point may cut off groups the new ranges cover.
				this.#resumed = true;
				this.#resolution = this.#fresh(ranges);
				for (let index = this.#copies.length - 1; index >= 0; index--) {
					if (this.#resolution.window.settled) break;
					const copy = this.#copies[index];
					// As in #window, a failed copy leaves the placement to the copy continuing the track.
					if (!this.route.concluded && copyFailed(copy)) continue;
					if (copy.from && sameRanges(copy.from, ranges)) this.#resolve(copy.resolved ?? []);
				}
			}
		}
		const cursor = this.#shell.cursor.peek();
		if (cursor === this.#bounded) return;
		const passed = this.#bounded.start;
		this.#bounded = cursor;
		for (const copy of [...this.#copies, ...this.#rerouted]) this.#bound(copy);
		if (cursor.start < passed) this.#backfill(cursor.start, passed);
		if (this.#ended) this.#letGo(this.#delivered);
	}

	/**
	 * Apply the reader's group cursor to `copy`, never below the copy's own resume point: groups
	 * under it were handed out by an earlier route, or never owed. A backfill has none: the
	 * reader's window and what it handed out decide what it takes.
	 */
	#bound(copy: Copy): void {
		const cursor = this.#shell.cursor.peek();
		const start = copy.backfill ? cursor.start : max(cursor.start, copy.floor ?? 0n);
		const replaced = copyEnd(copy);
		const end = replaced === undefined ? cursor.end : min(cursor.end ?? replaced, replaced);
		copy.sub.withRange({
			start: { included: start },
			end: end === undefined ? undefined : { excluded: end },
		});
	}

	/**
	 * Ask the newest copy's route for the groups from `start` up to `passed` under that copy's
	 * resume point, which a lowered floor newly covers. The copies that cached them may be gone,
	 * and that copy never asked for them, so a bounded copy of the same route asks for just those.
	 * One asked from a `raised` time start also reads the group holding it, under `start`.
	 */
	#backfill(start: bigint, passed: bigint, raised = false): void {
		const newest = this.#copies.at(-1);
		const resumed = newest && resumePoint(newest);
		if (this.#closed || this.#ended || !newest || resumed === undefined) return;
		const end = min(passed, resumed);
		// An ordered reader never goes back, nor does any reader past its budget.
		if (this.#delivered.covers(start, end)) return;
		// What a backfill of this route already failed to get, it would fail to get again.
		const refused = this.#refused;
		if (refused?.generation === newest.generation && refused.start <= start && end <= refused.end) return;
		// A floor lowered again asks once. A copy that dropped a group from `start` on no longer
		// offers it, so it asks for nothing there.
		const closed = newest.source.from.closed;
		const asks = (copy: Copy) => {
			const point = resumePoint(copy);
			return (
				copy.source.from.closed === closed && copy.floor !== undefined && point !== undefined && point <= start
			);
		};
		if (this.#copies.some((copy) => asks(copy) && (copyEnd(copy) ?? UNBOUNDED) >= end)) return;
		// A later resume point widens the route's backfill rather than asking again, so flaps over
		// a gap keep one per route, reading again one that ran out. Waited for as long as a new one
		// would be.
		const widened = this.#copies
			.slice(0, -1)
			.find((copy) => !(copy.done instanceof Error) && copy.until !== undefined && asks(copy));
		if (widened) {
			widened.until = end - 1n;
			widened.since = now();
			widened.done = undefined;
			if (raised) widened.backfill = true;
			this.#update(widened);
			this.#bound(widened);
			this.wake();
			return;
		}
		const backfill: Pick<Copy, "floor" | "backfill" | "until" | "from" | "resolved"> = {
			floor: start,
			backfill: raised ? true : undefined,
			until: end - 1n,
		};
		this.#add(
			{
				generation: newest.generation,
				source: newest.source,
				sub: subscribe(this.route.view(newest.source), this.route.name, this.#wire(backfill)),
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

	/**
	 * Ask the serving route for the groups below its copy's resume point that the reader still
	 * lacks: a late or reordered one, or one in flight on the route it replaced, as one cursor on
	 * the logical track would still get. From the lowest gap within the reader's cursor and window,
	 * which decide what it reads, so groups already handed out are not asked for again below it. A
	 * gap past the reader's budget was already forgotten as handed out. A lowered cursor asks
	 * through {@link #backfill} itself. A time start the resume point raised is asked from its time,
	 * so the backfill places it at the group holding it, as the route would for a plain reader.
	 * Mirrors the Rust `Reader::backfill_gaps`.
	 */
	#backfillGaps(delivered: Delivered): void {
		if (!this.#resumed) return;
		const window = this.#window();
		const raisedAt = this.#raised();
		// Kept until a copy places the ranges, but those whose start the resume point raised, which
		// only the backfill places: only then can the gaps they cover be judged.
		let floor = window.floor();
		const serving = this.#copies.at(-1);
		const resumed = serving && resumePoint(serving);
		if (floor === undefined && raisedAt.size !== window.unsettled().size) {
			// The gaps wait for the other ranges to place, but an owed raised start is asked now: its
			// backfill places it whatever they wait on, and a later ask finds it.
			if (resumed === undefined) return;
			let start: bigint | undefined;
			for (const time of raisedAt.values()) {
				if (!delivered.covers(time, resumed)) start = start === undefined ? time : min(start, time);
			}
			if (start !== undefined) this.#backfill(start, resumed, true);
			return;
		}
		if (floor === undefined) {
			if (raisedAt.size === 0) return;
			floor = window.runs()[0]?.start ?? 0n;
		}
		// A raised start whose groups from its time the reader already holds or passed owes nothing
		// under the resume point: placed at its time, it hands out what the group holding it would,
		// as none under it goes out.
		const owed: bigint[] = [];
		const placed: (Resolved | undefined)[] = [];
		const ranges = this.#ranges();
		for (const [key, time] of raisedAt) {
			if (resumed !== undefined && !delivered.covers(time, resumed)) {
				owed.push(time);
				continue;
			}
			const end = ranges[Number(key)]?.end;
			placed[Number(key)] = { start: time, end: end === undefined ? undefined : end + 1n, floored: true };
		}
		if (placed.length > 0) {
			this.#resolve(placed);
			// What the starts placed hold of the history a fold took unjudged is owed before the walk
			// below judges the gaps.
			this.#reopenFolded(delivered);
		}
		this.#resumed = false;
		if (resumed === undefined) return;
		// A group a replaced copy drains (see #mayDrain) is that copy's: the serving route is asked
		// from the lowest one none drains.
		const cursor = this.#shell.cursor.peek().start;
		// Each replaced copy's staleness verdict, taken once for the whole walk.
		const taking = new Map(
			this.#copies.filter((copy) => copy.replaced).map((copy) => [copy, hooks.taking(copy.sub)]),
		);
		// The copy left to drain group `key`, if any.
		const drains = (key: bigint) =>
			this.#copies.find((copy) => this.#mayDrain(copy, key, cursor, taking.get(copy)));
		let start: bigint | undefined;
		for (const gap of delivered.gaps(max(floor, cursor), resumed)) {
			let key = gap.start;
			for (; key < gap.end; key++) {
				const copy = drains(key);
				if (!copy) break;
				// Added to what earlier walks left.
				copy.drains ??= new Set();
				copy.drains.add(key);
			}
			if (key < gap.end) {
				start = key;
				break;
			}
		}
		// A raised start is asked from its time whatever the local floor, so the backfill places it,
		// and the group holding it is read once the floor lets it.
		for (const time of owed) start = start === undefined ? time : min(start, time);
		if (start === undefined) return;
		// The group holding a time start at or past where it is asked from may lie under it.
		const from = start;
		const raised = this.#ranges().some((range) => {
			const time = rangeTime(range);
			return time !== undefined && from <= time && time < resumed;
		});
		this.#backfill(start, resumed, raised);
	}

	/** Wake parked reads, so they look again and let go of what nothing needs. */
	wake(): void {
		this.#changed.update((n) => n + 1);
	}

	/** The newest open copy this reader holds of `source`'s broadcast. */
	copyOn(source: Source): Copy | undefined {
		return this.#copies.findLast(
			(copy) => copy.source.from.closed === source.from.closed && copy.sub.closed.peek() === undefined,
		);
	}

	/**
	 * The reader's copy of the route at `generation`, if it subscribed one, rather than a backfill of
	 * it: once the track ended, the one rerouted for the groups still read.
	 */
	copy(generation: number): Copy | undefined {
		const matches = (copy: Copy) => copy.generation === generation;
		return this.#copies.findLast(matches) ?? this.#rerouted.findLast(matches);
	}

	/**
	 * The copies other than `except` whose track still holds group `key` unfailed, newest first: the
	 * rerouted ones too, which continue a group though they hand none out.
	 */
	holding(key: bigint, except: Copy): Copy[] {
		const copies = [...this.#copies, ...this.#rerouted];
		const holding: Copy[] = [];
		for (let index = copies.length - 1; index >= 0; index--) {
			const copy = copies[index];
			if (copy !== except && hooks.holdsGroup(copy.sub, key)) holding.push(copy);
		}
		return holding;
	}

	cachedGroup(key: bigint, from = 0): GroupConsumer | undefined {
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			const group = hooks.cachedGroup(copy.sub, key, from);
			if (group) return this.#handOut(copy, group);
		}
		return undefined;
	}

	// A group a replaced copy delivered landed all the same, so every copy counts.
	landed(start: bigint, end: bigint): Instant | undefined {
		let landed: Instant | undefined;
		for (const copy of this.#copies) {
			const arrived = hooks.landed(copy.sub, start, end);
			if (arrived && (!landed || arrived.gt(landed))) landed = arrived;
		}
		return landed;
	}

	judgedGroup(key: bigint): GroupConsumer | undefined {
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			const group = hooks.cachedGroup(copy.sub, key);
			if (!group) continue;
			// Judged by the copy it is read from: this reader's own timeline stays empty.
			hooks.expireGroup(group, hooks.staleAt(copy.sub, key));
			return this.#handOut(copy, group);
		}
		return undefined;
	}

	/**
	 * `copy` handed out `key`. The serving copy's first group bounds the incumbent copies a switch
	 * left uncapped, just below it, unless they go (see {@link #delivering}): one a reader joining
	 * mid-switch reads (see {@link #joinIncumbent}), and one a switch before any group kept. What
	 * they hold under it may be groups the new route never has, and what is at or above it the new
	 * route delivers. Never below a group already handed out. A group it replays, already handed
	 * out, bounds nothing: the incumbent may still hold the next. Nor does a backfill of the
	 * serving route.
	 */
	#observe(copy: Copy, key: bigint): void {
		copy.delivered = true;
		if (copy.replaced || copy.generation !== this.#generation) return;
		if (this.#delivering(copy)) return;
		const bound = max(this.#newest ?? -1n, key - 1n);
		for (const replaced of this.#copies) {
			if (replaced === copy || !replaced.replaced || replaced.until !== undefined) continue;
			replaced.until = bound < 0n ? null : bound;
			replaced.since = now();
			this.#update(replaced);
			this.#bound(replaced);
		}
	}

	/**
	 * Serving `copy` delivers, which is when the Rust front splices it in. With nothing held nor
	 * handed out, the replaced copies a switch left uncapped have nothing to drain, so they go, as
	 * the Rust `Reader::sync` drops them on a switch without a boundary. Whether they went.
	 */
	#delivering(copy: Copy): boolean {
		this.route.delivered(copy.source);
		if (this.#newest !== undefined) return false;
		if (this.#copies.some((other) => other !== copy && hooks.latestKey(other.sub) !== undefined)) return false;
		this.#dropCopies((other) => other.replaced && other.until === undefined);
		return true;
	}

	/** Let go of every copy `drops` picks, learning first any end one declared, which outlives it. */
	#dropCopies(drops: (copy: Copy) => boolean): void {
		this.#learnEnd();
		this.#copies = this.#copies.filter((copy) => {
			if (!drops(copy)) return true;
			this.#drop(copy);
			return false;
		});
	}

	#deliverable(copy: Copy, key: bigint, delivered: Delivered): boolean {
		const end = copyEnd(copy);
		if (end !== undefined && key >= end) return false;
		// A range a copy placed during this read may hold a group a fold counted handed out.
		if (delivered.unjudged(key) && this.#window().holds(key)) this.#reopenFolded(delivered);
		return !delivered.has(key);
	}

	// Whether a copy's next group, `key`, waits for the reader's window to be placed, which decides
	// whether it goes out: a failed copy's, until a copy continuing the track places it, and a
	// backfill's that no range placed so far holds, since only a placed range bounds the groups under
	// the resume point. Mirrors the Rust `Reader::poll_first`.
	#waits(copy: Copy, key: bigint): boolean {
		if (!this.#mayWait(copy)) return false;
		const failed = !this.route.concluded && copyFailed(copy);
		const window = this.#window();
		return window.pending && (failed || !window.holds(key));
	}

	// Whether the hold in `#waits` can apply to a copy at all, so only those peek before taking.
	#mayWait(copy: Copy): boolean {
		return (!this.route.concluded && copyFailed(copy)) || copy.backfill === true;
	}

	tryGroup(ordered: boolean, delivered: Delivered, dispositions: boolean, admit?: bigint): Recv {
		this.#delivered = delivered;
		this.sync();
		this.#learnEnd();
		if (this.#closed) return { kind: "done" };
		this.#groupsRead = true;
		this.#reopenFolded(delivered);
		this.#reopen();
		this.#backfillGaps(delivered);
		if (dispositions) {
			const dropped = this.#tryDrop(delivered);
			if (dropped) return dropped;
		}
		// The first group any copy holds next: the lowest for an ordered read, else the first in
		// the subscription's Group Order. A replaced copy's buffered group comes before a later one
		// the serving copy already has. An arrival read still delivers a lower group that truly
		// arrives later.
		// A copy no switch replaced refusing the ranges fails the read before any copy hands out a
		// group, as the Rust `Reader::poll_first` meets it in the same pass as the groups: the serving
		// copy, or the last one while the front parks between routes.
		for (const copy of this.#copies) {
			if (copy.replaced || !this.#feeds(copy)) continue;
			const refused = hooks.refused(copy.sub);
			if (refused) return { kind: "error", error: refused, refused: true };
		}
		let owing: Copy[];
		for (;;) {
			const first = this.#first(ordered, delivered, admit);
			owing = first.owing;
			const next = first.next;
			if (!next) break;
			const recv = this.#take(next, ordered, dispositions, admit);
			if (recv.kind === "drop") {
				const dropped = this.#tryDrop(delivered);
				if (dropped) return dropped;
				continue;
			}
			if (recv.kind === "group") {
				// The take may pass the offered group, such as one missing its head, for a later one
				// the window leaves out: judge the group it got.
				const key = recv.group.start;
				const deliverable = this.#deliverable(next, key, delivered);
				if (!deliverable || !this.#inWindow(key)) {
					if (deliverable) this.#skip(next, key);
					recv.group.close();
					continue;
				}
				this.#observe(next, key);
				return { kind: "group", group: this.#handOut(next, recv.group) };
			}
			// The lowest buffered group keeps its place while its copy waits on a hole.
			// A newer copy must not pass it and permanently discard it from sequence order.
			if (ordered && recv.kind === "idle") return recv;
			break;
		}
		// The failed copies holding a group back for a placement, which still report their failure.
		const held = new Set<Copy>();
		// Whether a replaced copy failed this pass, owing the serving route a backfill.
		let owed = false;
		// The serving copy first: it is the one that keeps delivering.
		for (let index = this.#copies.length - 1; index >= 0; index--) {
			const copy = this.#copies[index];
			if (!this.#feeds(copy) || owing.includes(copy)) continue;
			for (;;) {
				const newest = this.#newest === undefined ? undefined : this.#newest + 1n;
				// Its group stays there until placed, but how it ran out is still judged below. It got
				// a group, so its route did not refuse the track.
				const peeked = this.#mayWait(copy) ? hooks.peekGroup(copy.sub, ordered, newest) : undefined;
				if (peeked !== undefined && this.#waits(copy, peeked)) {
					copy.delivered = true;
					held.add(copy);
					break;
				}
				const recv = this.#take(copy, ordered, dispositions, admit);
				if (recv.kind === "drop") {
					const dropped = this.#tryDrop(delivered);
					if (dropped) return dropped;
					continue;
				}
				if (recv.kind === "group") {
					const key = recv.group.start;
					const deliverable = this.#deliverable(copy, key, delivered);
					if (!deliverable || !this.#inWindow(key)) {
						if (deliverable) this.#skip(copy, key);
						recv.group.close();
						continue;
					}
					this.#observe(copy, key);
					return { kind: "group", group: this.#handOut(copy, recv.group) };
				}
				// Drained: the track ended as the copy did, which is not cleanly when its
				// session went away short of the declared end.
				if (recv.kind === "done") {
					const closed = copy.sub.closed.peek();
					copy.done = closed instanceof Error ? closed : hooks.arrived(copy.sub);
					if (!(closed instanceof Error) && hooks.subscriberEnd(copy.sub)) this.#settled = true;
				} else if (recv.kind === "error") {
					// A copy still serving that refuses the reader's ranges fails the read until they
					// change, as the read that met it says, so it is not done; a copy it replaced refusing
					// leaves the rest to the one now serving. Mirrors the Rust `Reader::poll_first`.
					if (recv.refused && !copy.replaced) return recv;
					copy.done = recv.error;
				} else if (this.#drained(copy, delivered)) copy.done = hooks.arrived(copy.sub);
				// What a replaced copy that just failed still had to drain is asked of the serving route.
				if (copy.replaced && copy.done instanceof Error) {
					this.#resumed = true;
					owed = true;
					// A backfill of the serving route that failed is not asked of that route again.
					const end = copyEnd(copy);
					if (copy.generation === this.#generation && copy.floor !== undefined && end !== undefined) {
						this.#refused = { generation: copy.generation, start: copy.floor, end };
					}
				}
				break;
			}
		}
		const resumed = this.#resumed;
		// Nothing to hand out, so a group a copy was left to drain and no longer drains is asked of
		// the serving route now, as a cursor asks a fill only once it holds nothing to read: a
		// backlog draining meanwhile costs no rescan per group.
		this.#rescanDrains(delivered);
		this.#letGo(delivered);
		// A copy that stopped draining a group, or was let go with one, owes the serving route a
		// backfill: ask now.
		if (this.#resumed && !resumed) owed = true;
		// A replaced copy that failed or was let go with a group to drain this pass owes the
		// serving route a backfill once the window is placed: ask now, before any end, since its
		// close already fired and no later group is sure to wake the read. A backfill that failed
		// is not asked again (`#refused`), so this ends. Mirrors the Rust `Reader::poll_first`.
		if (owed && this.#resumed && this.#window().floor() !== undefined) {
			return this.tryGroup(ordered, delivered, dispositions, admit);
		}
		// A copy that ran out or was let go no longer holds back a drop.
		if (dispositions) {
			const dropped = this.#tryDrop(delivered);
			if (dropped) return dropped;
		}
		const generation = this.route.generation;
		const end = this.#end((copy) => {
			if (typeof copy.done === "number") return null;
			const failed = held.has(copy) ? copy.sub.closed.peek() : undefined;
			return copy.done ?? (failed instanceof Error ? failed : undefined);
		});
		// A revert serves the copy the failed replacement capped again, which may hold the next
		// group already.
		if (end.kind === "idle" && this.route.generation !== generation) {
			return this.tryGroup(ordered, delivered, dispositions, admit);
		}
		return end;
	}

	/**
	 * Whether the reader's bounded {@link #window} has nothing left to deliver, once no replaced
	 * copy may still deliver a group under its bound: every group of the whole window is handed out
	 * (in `delivered`) or accounted for, as {@link #accounts} judges. Mirrors the Rust
	 * `Reader::exhausted`.
	 */
	exhausted(delivered: Delivered, ordered: boolean): boolean {
		const serving = this.#judging(delivered);
		if (!serving) return false;
		// The reader's own window, as its copies placed it: the subscriber's leaves its anchors to them.
		const window = this.#window();
		delivered.reopen(window);
		const end = window.end();
		if (end === undefined || window.pending) return false;
		const gaps = window.runs(window.floor() ?? 0n, end).flatMap((run) => [...delivered.gaps(run.start, run.end)]);
		return this.#accounts(serving, delivered, gaps, ordered);
	}

	/**
	 * Whether every key of `gaps`, which `delivered` lacks, is accounted for, once no replaced copy
	 * may still deliver a group under its bound; see {@link #accounts}.
	 */
	accounts(delivered: Delivered, gaps: readonly Run[], ordered: boolean): boolean {
		const serving = this.#judging(delivered);
		return serving !== undefined && this.#accounts(serving, delivered, gaps, ordered);
	}

	/**
	 * The serving copy, once the reader may judge what is accounted for: a route between copies,
	 * a backfill still to ask, or a replaced copy that may still deliver leaves nothing to judge.
	 * Lets go of the copies nothing needs any more either way.
	 */
	#judging(delivered: Delivered): Copy | undefined {
		this.sync();
		if (this.#closed || this.#ended || this.#resumed) return undefined;
		const serving = this.#copies.at(-1);
		if (!serving || serving.replaced || serving.done !== undefined || serving.generation !== this.#generation) {
			return undefined;
		}
		return this.#letGo(delivered) ? undefined : serving;
	}

	/**
	 * Whether every key of `gaps`, which `delivered` lacks, is accounted for.
	 *
	 * Under the serving copy's resume point, a key the cursor admits was the copies' it replaced, each
	 * let go once it had nothing more. One it keeps out stays owed, as on one cursor, until a copy held
	 * now declares it skipped or the asked copy's fills give it up, and the serving route is asked for those no copy does (see
	 * {@link #ask}), so its answer settles them however many copies came and went. The serving copy
	 * judges the rest by where it placed the reader's ranges itself: a time a copy held no group for
	 * places the reader's window at the time itself, wider than any group the serving copy reads.
	 * Mirrors the Rust `Reader::accounts`.
	 */
	#accounts(serving: Copy, delivered: Delivered, gaps: readonly Run[], ordered: boolean): boolean {
		const window = this.#window();
		if (window.end() === undefined || window.pending) return false;
		const resumed = resumePoint(serving) ?? 0n;
		const open: Run[] = [];
		const above: Run[] = [];
		for (const gap of gaps) {
			for (const run of this.#keptOut(gap, resumed)) {
				for (const unknown of this.#unskipped(run)) insertRun(open, unknown.start, unknown.end);
			}
			const split = min(max(resumed, gap.start), gap.end);
			if (split < gap.end) above.push({ start: split, end: gap.end });
		}
		if (open.length > 0) {
			// What the asked copy's fills gave up is accounted for, as on one cursor.
			let owed: Run[] = open;
			for (const gone of this.#ask(open)) owed = removeRun(owed, gone.start, gone.end);
			if (owed.length > 0) return false;
		}
		return hooks.accountsFor(serving.sub, delivered, above, ordered);
	}

	/**
	 * The parts of `gap` under the serving copy's resume point `resumed` that the cursor keeps out:
	 * owed, as on one cursor, until a copy declares them skipped. Mirrors the Rust `Reader::kept_out`.
	 */
	#keptOut(gap: Run, resumed: bigint): Run[] {
		const cursor = this.#shell.cursor.peek();
		const split = min(max(resumed, gap.start), gap.end);
		return [
			{ start: gap.start, end: min(split, cursor.start) },
			{ start: cursor.end === undefined ? split : max(cursor.end, gap.start), end: split },
		].filter((run) => run.start < run.end);
	}

	/** The parts of `run` neither recorded nor declared skipped by a copy held now. */
	#unskipped(run: Run): Run[] {
		const held = new Delivered();
		for (const skip of this.skippedIn(run.start, run.end)) held.addRun(skip.start, skip.end);
		return [...held.gaps(run.start, run.end)];
	}

	/**
	 * Ask the serving route for the hull of `open`, the keys the cursor keeps out that are neither
	 * recorded nor declared skipped by a copy held now. Its answer settles each such key as it would on one
	 * cursor: a skip, or a fill that gives it up, accounts for it, while a group or a drop leaves it owed. A copy of that route
	 * already asked for them, kept by {@link #letGo} until {@link #skipped} records them, asks nothing
	 * more, so neither a reader waiting on the answer nor one a group holds open asks again. Its
	 * track may forget a skip it answered before the rest lands, so it asks its fills for those it
	 * cannot prove, returning the keys of `open` those fills gave up. Mirrors the Rust `Reader::ask`.
	 */
	#ask(open: Run[]): Run[] {
		const givenUp: Run[] = [];
		const first = open.at(0);
		const last = open.at(-1);
		if (!first || !last) return givenUp;
		const keys = { start: first.start, end: last.end };
		this.#backfill(keys.start, keys.end);
		const serving = this.#copies.at(-1);
		if (!serving) return givenUp;
		for (const copy of this.#copies.slice(0, -1)) {
			const end = copyEnd(copy);
			if (copyAskedFor(copy, serving, keys.start) && end !== undefined && end >= keys.end) {
				for (const gone of hooks.askUnproven(copy.sub, open)) insertRun(givenUp, gone.start, gone.end);
			}
		}
		return givenUp;
	}

	tryDrop(delivered: Delivered, passing?: bigint): (Recv & { kind: "drop" }) | undefined {
		this.#delivered = delivered;
		this.sync();
		this.#learnEnd();
		return this.#tryDrop(delivered, passing);
	}

	/**
	 * The next run the serving copy gave up that `delivered` lacks and no other copy may still
	 * deliver. Only the serving copy's verdicts count; a copy it replaced may still deliver what
	 * is under its bound, so a run there waits until that copy no longer feeds. Mirrors the
	 * Rust `Reader::poll_drop`.
	 */
	#tryDrop(delivered: Delivered, passing = 0n): (Recv & { kind: "drop" }) | undefined {
		if (this.#closed) return undefined;
		// Below what the reader handed out or passed, no copy's drop is reported, so each copy's
		// record folds there, even one that hands out nothing while the reader returns gaps.
		const at = delivered.runEnd(0n) ?? 0n;
		for (const copy of this.#copies) hooks.fold(copy.sub, at);
		const serving = this.#serving();
		if (serving) {
			// A received stale run queued here that a new upstream subscription superseded since is
			// unknown again: the copy no longer holds it.
			for (const run of this.#drops) {
				if (!run.received) continue;
				const held = hooks.staleHeld(serving.sub, run.start, run.end);
				for (const gap of runGaps(held, run.start, run.end))
					this.#drops = removeRun(this.#drops, gap.start, gap.end);
			}
			for (let dropped = hooks.tryDrop(serving.sub); dropped; dropped = hooks.tryDrop(serving.sub)) {
				this.#queueDrop(serving, dropped);
			}
		}
		this.#drops = this.#drops.filter((run) => !delivered.covers(run.start, run.end));
		const cursor = this.#shell.cursor.peek();
		const feeding: Run[] = [];
		for (const copy of this.#copies) {
			if (copy === serving || !this.#feeds(copy)) continue;
			const start = max(cursor.start, copy.floor ?? 0n);
			insertRun(feeding, start, copyEnd(copy) ?? UNBOUNDED);
		}
		for (const run of this.#drops) {
			const end = min(run.end, cursor.end ?? run.end);
			for (const gap of delivered.gaps(max(run.start, cursor.start), end)) {
				const [open] =
					gap.start < passing
						? [{ start: gap.start, end: min(gap.end, passing) }]
						: runGaps(feeding, gap.start, gap.end);
				if (open) {
					// Report once without marking it delivered: a late fill may still land.
					this.#drops = removeRun(this.#drops, open.start, open.end);
					return { kind: "drop", ...open, error: run.error, skip: run.skip, stale: run.stale };
				}
			}
		}
		return undefined;
	}

	// Queue a run the serving copy gave up, to report once no other copy may still deliver it.
	#queueDrop(serving: Copy, dropped: Recv & { kind: "drop" }): void {
		// A route failing is no verdict: another route may still deliver the group, and one it
		// failed while buffered is handed out to continue through its recovery.
		if (routeFailed(dropped.error)) return;
		// A stale run the copy holds came from its upstream, which a new subscription may supersede.
		const received =
			dropped.stale !== undefined && hooks.staleHeld(serving.sub, dropped.start, dropped.end).length > 0;
		this.#drops = insertDrop(this.#drops, dropped.start, dropped.end, { ...dropped, received });
	}

	// Take `copy`'s next group past the newest handed out. Under `dispositions`, the serving copy
	// returns first what its budget skips on the way, which is queued, so a publisher hears of it
	// before the group, as from a direct reader; only the serving copy's verdicts count. A failed
	// group still comes out, to continue through its recovery.
	#take(copy: Copy, ordered: boolean, dispositions: boolean, admit?: bigint): Recv {
		const newest = this.#newest === undefined ? undefined : this.#newest + 1n;
		const serving = dispositions ? this.#serving() : undefined;
		const recv = take(copy, ordered, newest, copy === serving && "budget", admit);
		if (recv.kind === "drop" && serving) this.#queueDrop(serving, recv);
		return recv;
	}

	// Whether `copy`, bounded where it was replaced, ran out under that bound: its track ended, no
	// fill still owes it a group, and it holds nothing there unread. Its subscriber would park on a
	// group past the bound instead, as one past a local cap waits for it to rise, but the reader
	// moves this bound itself by asking again. Mirrors the Rust arrival cursor ending on a complete
	// track. Judged right after a read, which records whether it waits on a fill.
	#drained(copy: Copy, delivered: Delivered): boolean {
		const end = copyEnd(copy);
		if (end === undefined || hooks.awaitsFill(copy.sub)) return false;
		const start = max(this.#shell.cursor.peek().start, copy.floor ?? 0n);
		return hooks.spent(copy.sub, start, end, delivered);
	}

	// A copy that ran out cleanly still takes a group in flight below its end while the read stays
	// open, as a cursor still reading it would. One landing on the serving copy may lie under its
	// resume point, which is not that copy's to read, and a backfill that already ran out was let
	// go: ask its route again. Mirrors the Rust `Reader::poll_first`.
	#reopen(): void {
		const serving = this.#serving();
		for (const copy of this.#copies) {
			if (typeof copy.done !== "number" || hooks.arrived(copy.sub) <= copy.done) continue;
			copy.done = undefined;
			if (copy === serving && !copy.replaced && copy.floor !== undefined) this.#resumed = true;
		}
	}

	// Re-arm the walk once a group a walk left a copy to drain, which the reader still lacks, no
	// longer drains there: evicted, given up as stale, or its copy failed or spent its budget. The
	// serving route is then asked for it after all, as a cursor on the track would ask again. Costs
	// the groups left to drain, each one a copy holds. Mirrors the Rust `Reader::rescan_drains`.
	#rescanDrains(delivered: Delivered): void {
		const cursor = this.#shell.cursor.peek().start;
		for (const copy of this.#copies) {
			if (copy.drains === undefined) continue;
			for (const key of copy.drains) if (delivered.has(key)) copy.drains.delete(key);
			const taking = copy.drains.size > 0 ? hooks.taking(copy.sub) : undefined;
			let drains = true;
			for (const key of copy.drains) {
				if (this.#mayDrain(copy, key, cursor, taking)) continue;
				drains = false;
				break;
			}
			if (drains) continue;
			copy.drains = undefined;
			this.#resumed = true;
		}
	}

	// Whether `copy` drains group `key` for a reader whose cursor starts at `cursor`: replaced and
	// still read, holding the group's head under its cap and inside its own ranges, and its read
	// `taking` still offers it. A copy that failed or spent its drain budget delivers nothing more,
	// even while a read still leases it, and one holding a group without its head may give that
	// group up, so neither drains. Mirrors the Rust `Copy::may_drain`.
	#mayDrain(copy: Copy, key: bigint, cursor: bigint, taking: ((key: bigint) => boolean) | undefined): boolean {
		return (
			copy.replaced &&
			this.#feeds(copy) &&
			(copy.backfill ? cursor : max(cursor, copy.floor ?? 0n)) <= key &&
			key < (copyEnd(copy) ?? UNBOUNDED) &&
			taking?.(key) === true &&
			!copyFailed(copy) &&
			hooks.holdsGroup(copy.sub, key, true)
		);
	}

	// Whether `copy` may still feed groups: its channel has not run out, and what it holds is
	// within the reader's budget, which ends a full budget after a newer group superseded it.
	#feeds(copy: Copy): boolean {
		return copy.done === undefined && !this.#expired(copy, this.#budget());
	}

	// `copy` gave up `key`, a group outside the reader's window, for good: a widened window asks
	// its route again for it.
	#skip(copy: Copy, key: bigint): void {
		copy.dropped = max(copy.dropped ?? 0n, key + 1n);
	}

	// The copy whose next group is the first deliverable one in the reader's window, dropping any
	// it holds next that are not, and the copies whose ordered cursor is left on such a group
	// because the window still owes a lower one that may yet land there. First is the lowest for an
	// ordered read, else the first in the subscription's Group Order. Mirrors the Rust `poll_first`.
	// A group at or past `admit` (ordered reads only) stays next, unjudged.
	#first(ordered: boolean, delivered: Delivered, admit?: bigint): { next?: Copy; owing: Copy[] } {
		let first: { copy: Copy; key: bigint } | undefined;
		const owing: Copy[] = [];
		const floor = ordered ? (delivered.runEnd(0n) ?? 0n) : 0n;
		const ascending = ordered || this.#shell.subscription.peek()?.order === "ascending";
		for (const copy of this.#copies) {
			if (!this.#feeds(copy)) continue;
			for (;;) {
				const key = hooks.peekGroup(
					copy.sub,
					ordered,
					this.#newest === undefined ? undefined : this.#newest + 1n,
				);
				// At or past `admit`, it stays next, unjudged.
				if (key === undefined || this.#waits(copy, key) || (admit !== undefined && key >= admit)) break;
				const deliverable = this.#deliverable(copy, key, delivered);
				if (deliverable && this.#inWindow(key)) {
					// A tie goes to the newer copy, so the serving one hands out its first group and
					// bounds the copies it replaced.
					if (!first || (ascending ? key <= first.key : key >= first.key)) first = { copy, key };
					break;
				}
				// Skipping an ordered cursor past a group the window still owes below would lose it.
				if (ordered && deliverable && this.#owesBelow(copy, floor, key, delivered)) {
					owing.push(copy);
					break;
				}
				const recv = take(copy, ordered, this.#newest === undefined ? undefined : this.#newest + 1n);
				if (recv.kind !== "group") break;
				if (deliverable) this.#skip(copy, recv.group.start);
				recv.group.close();
			}
		}
		return { next: first?.copy, owing };
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
			// A copy it replaced finished the track and the reader owes nothing below that end:
			// the track ended cleanly whoever serves it now. Mirrors the Rust front. The failing
			// copy's own end, or one whose copy failed short of it, never settled.
			const end = this.#learnEnd();
			const owed = max(this.#shell.cursor.peek().start, (this.#newest ?? -1n) + 1n);
			const settled =
				this.#settled ||
				this.#copies.some(
					(copy) =>
						copy !== serving &&
						hooks.subscriberEnd(copy.sub) !== undefined &&
						hooks.peekLargest(copy.sub).ended &&
						!(copy.sub.closed.peek() instanceof Error),
				);
			if (end && settled && !this.#declaredError && finalKey(end) <= owed)
				return this.#conclude() ? { kind: "done" } : { kind: "idle" };
			if (serving && this.#revert(serving, ended)) return { kind: "idle" };
			// A refusal from a route that still serves is the track's answer.
			if (!routeFailed(ended)) return this.#fail(ended);
			if (route.refusal) return this.#fail(route.refusal);
			// The route that failed it still serves and is not going away, so no move replaces
			// the copy. As in the Rust front, a received route that lost the content judged the
			// route, not the track: the read waits for the next route, since asking the same one
			// again could only replace the withdrawal's cause with a refusal, and that route's
			// news follows on the same session. A local broadcast's word is the track's verdict.
			// Otherwise one that delivered nothing is the route refusing the track, and one that
			// did is subscribed afresh. A concluded route has no front left to move it, so its
			// loss ends the read.
			if (serving && route.source === serving.source && !closing(ended)) {
				if (!route.concluded && strands(ended, serving.source)) {
					route.strand(serving.source);
					return { kind: "idle" };
				}
				if (!serving.delivered) {
					route.refuse(ended);
					return this.#fail(ended);
				}
				if (unroutable(ended)) return this.#fail(ended);
				route.resubscribe();
			}
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

	/**
	 * A replacement that failed before its copy held a group never ends the track while a copy it
	 * replaced still feeds it: that copy serves again, and the failure is the replacement refusing
	 * the track unless its route is going away. Judged by the copy, whatever this reader's window
	 * took from it, as the Rust front does.
	 */
	#revert(serving: Copy, failed: Error): boolean {
		if (hooks.latestKey(serving.sub) !== undefined) return false;
		// One that ran out under its cap still serves once the revert lifts the cap.
		const incumbent = this.#copies.findLast(
			(copy) =>
				copy.replaced &&
				copy.source !== serving.source &&
				!(copy.done instanceof Error) &&
				copy.sub.closed.peek() === undefined,
		);
		if (!incumbent || !this.route.revert(serving.source, incumbent.source)) return false;
		if (!closing(failed)) {
			// A replacement losing its route judged the route: the replaced copy keeps feeding the
			// read, which asks the replacement again once the table moves, as the Rust front does.
			if (strands(failed, serving.source)) this.route.strand(serving.source);
			else this.route.refuse(failed);
		}
		return true;
	}

	#fail(error: Error): Recv & { kind: "error" } {
		this.#shell.close(error);
		return { kind: "error", error };
	}

	/**
	 * End the logical track cleanly, once its serving copy did and nothing it replaced may still
	 * deliver a group below that end: settle `closed`, keeping each copy only while it holds
	 * something left to read. The other channel's backlog stays readable. The reader follows the
	 * route only for the groups still read, and {@link #unroute} leaves it once it reads and holds
	 * nothing. Whether the track ended.
	 */
	#conclude(): boolean {
		if (this.#closed) return false;
		if (this.#ended) return true;
		// Learned before any copy that knows it goes.
		this.#learnEnd();
		const serving = this.#serving();
		if (serving) this.#boundAtEnd(serving);
		const resumed = this.#resumed;
		// A group a copy no longer drains, which no read since has asked of the serving route.
		this.#rescanDrains(this.#delivered);
		if (this.#letGo(this.#delivered) || this.#closed) return false;
		// A copy that stopped draining a group, or was let go with one, owes the serving route a
		// backfill first: asked here, since a caller awaiting only the close may never read, and
		// the end then waits for it as for any copy that may still deliver.
		if (this.#resumed && !resumed && this.#window().floor() !== undefined) {
			this.#backfillGaps(this.#delivered);
			this.wake();
			return false;
		}
		this.#ended = {
			largest: serving && hooks.peekLargest(serving.sub).largest,
			object: serving && hooks.objectLargest(serving.sub),
			ended: true,
		};
		this.#shell.close();
		this.#letGo(this.#delivered);
		this.wake();
		return true;
	}

	/**
	 * Drop the copies nothing needs any more. A replaced one goes by {@link #retains}. One not yet
	 * replaced stays, even between routes: it is still the newest copy there is, until the track
	 * ended and it holds nothing left to read. Whether a group the reader lacks may still land on
	 * a replaced one, which a clean end waits for.
	 *
	 * So does the copy of the serving route asked furthest from where {@link #asking} starts: it is
	 * the ask {@link #accounts} makes, waiting on that route's answer, and letting it go would only
	 * ask again. Only that one, since every ask starts there: an ask from lower down covers what the
	 * copies asked before it did, so a range moved again and again keeps one.
	 */
	#letGo(delivered: Delivered): boolean {
		// An end a copy declared outlives the copy.
		this.#learnEnd();
		if (this.#closed) return false;
		const last = this.#copies.at(-1);
		const edge = last && hooks.latestKey(last.sub);
		const cursor = this.#shell.cursor.peek();
		const asking = this.#asking(delivered);
		let asked: Copy | undefined;
		if (asking !== undefined && last && !last.replaced) {
			for (const copy of this.#copies) {
				if (!copy.replaced || !copyAskedFor(copy, last, asking)) continue;
				if (!asked || (copyEnd(copy) ?? UNBOUNDED) >= (copyEnd(asked) ?? UNBOUNDED)) asked = copy;
			}
		}
		let waits = false;
		this.#copies = this.#copies.filter((copy) => {
			if (!copy.replaced) {
				if (!this.#ended || copy.lease > 0) return true;
				const start = max(cursor.start, copy.floor ?? 0n);
				if (!hooks.spent(copy.sub, start, undefined, delivered)) return true;
				// A group read from a copy it replaced may still continue from it.
				if (this.#backs(copy, true, grace(Duration.millis(this.#budget())))) return true;
			} else {
				const retains = this.#retains(copy, delivered, edge);
				if (retains === "wait") waits = true;
				if (retains || copy === asked) return true;
				// Let go with a group a walk left it to drain, which it gave up by its own verdict:
				// the serving route is asked for it after all, as when the copy's cache loses it.
				if ([...(copy.drains ?? [])].some((key) => !delivered.has(key))) this.#resumed = true;
			}
			this.#drop(copy);
			return false;
		});
		this.#unroute();
		return waits;
	}

	/**
	 * Where {@link #accounts} asks the serving route from: the first key of a bounded window under
	 * the serving copy's resume point that `delivered` lacks and the cursor keeps out, past the
	 * skipped run {@link #skipped} records from the start of each run of them. Records those runs
	 * first, from what every copy held now declares, so an answer is kept before the cache that holds
	 * it forgets it. A reader whose cursor keeps nothing out, or that follows one route, pays
	 * nothing. Mirrors the Rust `Reader::asking`.
	 */
	#asking(delivered: Delivered): bigint | undefined {
		const serving = this.#copies.at(-1);
		const cursor = this.#shell.cursor.peek();
		const window = this.#window();
		const resumed = serving && !serving.replaced ? resumePoint(serving) : undefined;
		if (resumed === undefined || window.end() === undefined || (cursor.start === 0n && cursor.end === undefined)) {
			this.#skipped = new Delivered();
			return undefined;
		}
		const kept = new Delivered();
		let asking: bigint | undefined;
		for (const run of window.runs(window.floor() ?? 0n, resumed)) {
			for (const gap of delivered.gaps(run.start, run.end)) {
				for (const out of this.#keptOut(gap, resumed)) {
					const first = this.skippedIn(out.start, out.end)[0];
					const start = first?.start === out.start ? first.end : out.start;
					if (start > out.start) kept.addRun(out.start, start);
					if (start < out.end) asking ??= start;
				}
			}
		}
		this.#skipped = kept;
		return asking;
	}

	/**
	 * How the reader still needs replaced `copy`: `"wait"` while a group the reader lacks may still
	 * land on it, `"keep"` while it holds one unread or backs a group handed out. `edge` is the
	 * newest group the serving copy has.
	 */
	#retains(copy: Copy, delivered: Delivered, edge: bigint | undefined): "wait" | "keep" | undefined {
		// Until the first group read, no cached copy needs to be retained.
		if (!this.#groupsRead) return undefined;
		// An incumbent not yet bounded may still feed a group the new copy never has.
		if (copy.until === undefined && copy.done === undefined) return "wait";
		const end = copyEnd(copy);
		// Superseded once the serving copy has a group past the bound.
		if (copy.since === undefined && end !== undefined && edge !== undefined && edge >= end) copy.since = now();
		const budget = this.#budget();
		// One whose wire names every start waits for it to, however long that takes.
		const wait = hooks.provenTrack(copy.sub) ? undefined : grace(Duration.millis(budget));
		// What is under the bound is past the reader's budget a full budget after a newer group
		// superseded it, landed or not.
		if (copy.done === undefined && end !== undefined && !this.#expired(copy, budget)) {
			const cursor = this.#shell.cursor.peek();
			// From where it was asked to start, or else from what it first got. A backfill owes what
			// the reader's window hands out below its floor too.
			const start = copy.backfill ? cursor.start : max(cursor.start, copy.floor ?? 0n);
			const floor = copy.floor !== undefined;
			// A group still to land in the reader's window is waited for, as a gap is, but not one a
			// copy declared skipped: its publisher never produced it, so no route will.
			const lands = (run: { start: bigint; end: bigint }) =>
				runGaps(this.skippedIn(run.start, run.end), run.start, run.end).some((open) =>
					hooks.mayLand(copy.sub, { ...open, floor, grace: wait, delivered }),
				);
			if (this.#window().runs(start, end).some(lands)) return "wait";
			if (hooks.hasUnreadGroup(copy.sub, cursor.start, end, delivered)) return "keep";
		}
		// Until the serving copy holds a group, a failure of it serves this one again; see #revert.
		// Only after a group still to land is waited for, so a clean end of the serving copy
		// waits for it too.
		const serving = this.#serving();
		if (
			serving &&
			serving.source !== copy.source &&
			hooks.latestKey(serving.sub) === undefined &&
			!(copy.done instanceof Error)
		) {
			return "keep";
		}
		if (copy.lease > 0) return "keep";
		const lands = !this.#expired(copy, wait?.asMillis() ?? Number.POSITIVE_INFINITY);
		return this.#backs(copy, lands, wait) ? "keep" : undefined;
	}

	/**
	 * Whether `copy` holds a group still read, or may yet get it within `wait` when it `lands`: a
	 * failure of the copy that group is read from continues here.
	 */
	#backs(copy: Copy, lands: boolean, wait: Duration | undefined): boolean {
		for (const recover of this.out) {
			const key = recover.key;
			if (hooks.holdsGroup(copy.sub, key)) return true;
			// A group at or past where it was asked to start may land even before its first
			// arrival; without a floor, only what follows its first arrival can.
			const asked = copy.floor !== undefined && (copy.backfill || copy.floor <= key);
			const land = { start: key, end: key + 1n, floor: asked, grace: wait };
			if (lands && hooks.mayLand(copy.sub, land)) return true;
		}
		return false;
	}

	/**
	 * `serving` ended cleanly, so nothing newer supersedes what the copies it replaced still carry
	 * under their caps: the budget for it runs from now. An incumbent not yet bounded is bounded at
	 * that end, so a group it still carries below the end is waited for and nothing past it.
	 */
	#boundAtEnd(serving: Copy): void {
		const declared = this.#declared;
		const until =
			declared !== undefined
				? finalKey(declared) - 1n
				: max(this.#newest ?? -1n, hooks.latestKey(serving.sub) ?? -1n);
		for (const copy of this.#copies) {
			if (!copy.replaced) continue;
			copy.since ??= now();
			if (copy.until !== undefined) continue;
			copy.until = until < 0n ? null : until;
			this.#update(copy);
			this.#bound(copy);
		}
	}

	/**
	 * The reader's widest budget in milliseconds, how far behind any group may fall before it is
	 * skipped, as its own ranges set it: a copy is let go as a whole, so it waits out the widest.
	 * Mirrors the Rust `Mirrored::widest`.
	 */
	#budget(): number {
		return widest(this.#ranges());
	}

	/**
	 * How long each group stays worth delivering, as the reader's own ranges set it, placed where its
	 * copies resolved them: the window places a live join as a time start, whose groups still go
	 * stale past its max. Mirrors the Rust `Reader::budget`.
	 */
	#readerBudget(): Budget {
		const window = this.#window();
		const ranges = new Map(this.#ranges().map((range, index) => [BigInt(index), range] as const));
		return window.budgetOf(ranges);
	}

	/**
	 * The groups the reader's ranges resolve to and the cap its cursor stops at, undefined while a
	 * live join waits on a first group. Mirrors the Rust `Recover::window`.
	 */
	coverage(): Coverage {
		const window = this.#window();
		if (window.pending) return undefined;
		return { runs: window.runs(), cap: this.#shell.cursor.peek().end };
	}

	/**
	 * Whether `within` passed since `copy` was superseded; see {@link Copy.since}. Arms a wakeup
	 * for then, so a read waiting on the copy looks again.
	 */
	#expired(copy: Copy, within: number): boolean {
		if (copy.since === undefined) return false;
		// Plain nanoseconds: an unenforced budget is infinite, which no Duration holds.
		const at = copy.since.since(Instant.ZERO).asNanos() + within * 1_000_000;
		if (now().since(Instant.ZERO).asNanos() >= at) return true;
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
		void sleepUntil(Instant.ZERO.saturatingAdd(Duration.nanos(at)), { signal: timer.signal, ref: false }).then(
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
		this.#newest = max(this.#newest ?? 0n, group.start);
		// Another copy holding the group, now or once it lands there, is where a failure continues
		// it, so it stays while the group is read; see #backs. Recorded even with one copy, since a
		// later route's copy may come to hold it.
		const recover = new Recover(this, group, copy);
		hooks.recoverGroup(group, recover);
		recover.watch(group);
		// Closed with the reader, as a direct reader's close reaches the groups it handed out.
		this.#open.add(group);
		void hooks.groupClosed(group).then(() => this.#open.delete(group));
		return group;
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
		for (const copy of this.#copies) disposes.push(hooks.groupChanged(copy.sub, once));
		return dispose;
	}

	wholeGroups(frameOf: (key: bigint) => number | undefined): void {
		this.#whole = frameOf;
		for (const copy of this.#copies) this.#readWhole(copy);
	}

	// Have `copy` read whole groups, except one the reader already handed out, as from a copy it
	// replaced: that group continues through its recovery, and no copy waits on its head.
	#readWhole(copy: Copy): void {
		const whole = this.#whole;
		if (whole) hooks.wholeGroups(copy.sub, (key) => (this.#delivered.has(key) ? undefined : whole(key)));
	}

	forgetPastBudget(delivered: Delivered): void {
		this.forgetPast(delivered, { budget: this.#readerBudget(), ranges: this.#placed() });
	}

	// The reader's window once nothing waits on a copy to place a live join.
	#placed(): Window | undefined {
		const window = this.#window();
		return window.pending ? undefined : window;
	}

	// Take back what a fold counted as handed out in `delivered` before any range held it, which the
	// reader's ranges, as its copies placed them so far, now owe. Each read does so before it judges
	// what the copies hold, since the copies place a range only once a group arrives for it.
	#reopenFolded(delivered: Delivered): void {
		delivered.reopen(this.#window());
	}

	untell(start: bigint, end: bigint): void {
		for (const copy of this.#copies) hooks.untell(copy.sub, start, end);
	}

	// Judged on the newest copy, but never past a group a replaced copy still feeding the reader,
	// or that would under a wider budget, holds unread: as one cursor keeps what it owes, since a
	// late group there may be fresh by its own successor on that copy. A widened budget reads an
	// expired copy again, so only a copy whose channel ran out is passed over, and only a group
	// stale even at the track's max age, which bounds every budget: a lease on the copy never
	// holds back the fold for good. Nor past `keep`, an outer reader's own such group.
	forgetPast(delivered: Delivered, terms: FoldTerms, keep?: bigint): void {
		const newest = this.#copies.at(-1);
		if (!newest) return;
		let kept = keep;
		for (const copy of this.#copies) {
			if (copy === newest || !copy.replaced || copy.done !== undefined) continue;
			const key = hooks.lowestBuffered(copy.sub, delivered, true);
			if (key !== undefined) kept = min(kept ?? key, key);
		}
		hooks.forgetPastBudget(newest.sub, delivered, terms, kept);
	}

	// The same key is the same group on every copy, so it bounds group `key` whichever copy holds it.
	keyAfter(key: bigint): bigint | undefined {
		let after: bigint | undefined;
		for (const copy of this.#copies) {
			const next = hooks.keyAfter(copy.sub, key);
			if (next !== undefined) after = min(after ?? next, next);
		}
		return after;
	}

	// A copy it replaced refusing leaves the rest to the copy now serving. Mirrors the Rust
	// `resume::Subscriber::refused`.
	refused(): Error | undefined {
		for (const copy of this.#copies) {
			if (copy.replaced) continue;
			const refused = hooks.refused(copy.sub);
			if (refused) return refused;
		}
		return undefined;
	}

	lowestBuffered(delivered: Delivered, fresh?: boolean): bigint | undefined {
		let lowest: bigint | undefined;
		for (const copy of this.#copies) {
			const key = hooks.lowestBuffered(copy.sub, delivered, fresh);
			if (key !== undefined) lowest = min(lowest ?? key, key);
		}
		return lowest;
	}

	latest(): bigint | undefined {
		let latest: bigint | undefined;
		for (const copy of this.#copies) {
			const key = hooks.latestKey(copy.sub);
			if (key !== undefined && (latest === undefined || key > latest)) latest = key;
		}
		return latest;
	}

	#serving(): Copy | undefined {
		const last = this.#copies.at(-1);
		return last?.generation === this.#generation ? last : undefined;
	}

	// The logical track's end, learning it from any copy that declared one.
	#learnEnd(): track.Position | undefined {
		for (const copy of this.#copies) {
			const end = hooks.subscriberEnd(copy.sub);
			if (!end) continue;
			this.#declare(end);
			// Only a copy at the end that stands says how the track ended there.
			const declared = this.#declared;
			const same =
				declared !== undefined &&
				((declared.group === end.group && declared.frame === end.frame) ||
					(end.frame === 0 && finalKey(end) === finalKey(declared)));
			if (same) this.#declaredError ??= hooks.subscriberEndError(copy.sub);
		}
		return this.#declared;
	}

	// Record `end` as the logical track's end, unless a copy declared one first. A mid-group end
	// refines the group boundary it falls just before, as a wire end may.
	#declare(end: track.Position): void {
		if (this.#closed) return;
		const declared = this.#declared;
		if (!declared) {
			this.#declared = end;
			this.wake();
		} else if (declared.frame === 0 && end.frame !== 0 && finalKey(declared) === finalKey(end)) {
			this.#declared = end;
		}
	}

	start(): bigint | "pending" | undefined {
		this.sync();
		const serving = this.#serving();
		return serving && hooks.sourceStart(serving.sub);
	}

	/** Whether the copy serving now places ranges as its publisher would; see `hooks.placesAsUpstream`. */
	placesAsUpstream(): boolean {
		this.sync();
		const serving = this.#serving();
		return serving !== undefined && hooks.placesAsUpstream(serving.sub);
	}

	/** Whether the copy serving now knows where the live feed is; see `hooks.knowsEdge`. */
	knowsEdge(): boolean {
		this.sync();
		const serving = this.#serving();
		return serving !== undefined && hooks.knowsEdge(serving.sub);
	}

	end(): track.Position | undefined {
		this.sync();
		return this.#learnEnd();
	}

	skippedBelow(key: bigint): bigint {
		this.sync();
		return this.#copies.reduce((longest, copy) => max(longest, hooks.skippedBelow(copy.sub, key)), 0n);
	}

	/** Whether a route's copy serves the reader now. */
	serving(): boolean {
		this.sync();
		return this.#serving() !== undefined;
	}

	/**
	 * Ask the serving copy's fills for `unproven`, kept-out keys a containing front's reader owes, as
	 * that reader asks a plain copy, returning what they gave up, so a key this front's route cannot
	 * fill ends the outer read too. Mirrors the Rust `Resume::ask_unproven`.
	 */
	askUnproven(unproven: Run[]): Run[] {
		const serving = this.#copies.at(-1);
		return serving ? hooks.askUnproven(serving.sub, unproven) : [];
	}

	// What the reader recorded or any copy held now declared skipped: a skip is final for the whole
	// track, so an outer front reading this one as a copy counts it, and asks for the rest as it would
	// of any route.
	skippedIn(start: bigint, end: bigint): Run[] {
		const skipped = new Delivered();
		for (const run of runsIn(this.#skipped, start, end)) skipped.addRun(run.start, run.end);
		for (const copy of this.#copies) {
			for (const run of copySkips(copy, start, end)) skipped.addRun(run.start, run.end);
		}
		return runsIn(skipped, start, end);
	}

	// A copy this front replaced may still land a group its serving copy never will, so an outer
	// front reading this one as a copy waits on every copy still kept, each past where it starts
	// and under its bound. Mirrors the Rust `Subscriber::may_land`.
	/** Whether the serving route's copy names every start on its wire. */
	proven(): boolean {
		const serving = this.#serving();
		return serving !== undefined && hooks.provenTrack(serving.sub);
	}

	mayLand(land: Land): boolean {
		for (const copy of this.#copies) {
			const start = copy.backfill || copy.floor === undefined ? land.start : max(land.start, copy.floor);
			const bound = copyEnd(copy);
			const end = bound === undefined ? land.end : min(land.end, bound);
			if (start < end && hooks.mayLand(copy.sub, { ...land, start, end })) return true;
		}
		return false;
	}

	// The serving route's copy holds what its publisher skipped or gave up.
	holeEnd(key: bigint, dropped: boolean): bigint | undefined {
		const serving = this.#serving();
		return serving && hooks.holeEnd(serving.sub, key, dropped);
	}

	wholeEnd(key: bigint): bigint | undefined {
		const serving = this.#serving();
		return serving && hooks.wholeEnd(serving.sub, key);
	}

	wholeAround(key: bigint): Run | undefined {
		const serving = this.#serving();
		return serving && hooks.wholeAround(serving.sub, key);
	}

	// A failed serving copy's own resolution is no placement while the front may still replace it:
	// the reader's ranges stay as placed, pending until a copy continuing the track places them.
	// Mirrors the Rust reader answering from its window.
	#placesNothing(serving: Copy): boolean {
		return !this.route.concluded && copyFailed(serving);
	}

	// Each copy asks for the reader's ranges in turn, clamped to its floor and replacement.
	starts(from: bigint): (bigint | undefined)[] | undefined {
		this.sync();
		const serving = this.#serving();
		if (!serving) return undefined;
		if (!this.#placesNothing(serving)) return hooks.servingStarts(serving.sub, from);
		return this.#window().starts(from);
	}

	// Where a live join this reader placed at `placed` starts under `budget` milliseconds, on the
	// serving copy. Mirrors the Rust `Subscriber::join_start` on a reader following route copies.
	joinStart(placed: bigint, budget: number): bigint {
		this.sync();
		const serving = this.#serving();
		return serving ? hooks.joinStart(serving.sub, placed, budget) : placed;
	}

	// The reader's ranges as its window resolved them, whichever copy serves.
	resolved(): (Resolved | undefined)[] {
		this.sync();
		return this.#window().resolved();
	}

	// The reader's own window, which spans every copy: the serving one starts at the switch's
	// boundary, so it leaves out what a replaced copy still delivers below it. Mirrors the Rust
	// `Resume::runs`.
	runs(start: bigint, end: bigint): Run[] {
		this.sync();
		return this.#window().runs(start, end);
	}

	// The reader's own window, synced, as `runs` reads it. Mirrors the Rust `Resume::with_window`.
	window(): Window {
		this.sync();
		return this.#window();
	}

	// The serving copy's runs leave out the keys below a time start it still waits to place, so its
	// barrier holds a gap the group containing that start may lie below.
	gapHeld(at: bigint, end: bigint): boolean {
		this.sync();
		const serving = this.#serving();
		return serving !== undefined && hooks.gapHeld(serving.sub, at, end);
	}

	servingEnd(): bigint | undefined {
		this.sync();
		const serving = this.#serving();
		return serving && hooks.servingEnd(serving.sub);
	}

	/** The error the logical track's end carries, if it does not end cleanly. */
	endError(): Error | undefined {
		this.sync();
		this.#learnEnd();
		return this.#declaredError;
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
			if (serving && failed instanceof Error) {
				if (this.#revert(serving, failed)) continue;
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

	watchError(fn: (error: Error) => void): Dispose {
		let stop: Dispose = () => {};
		const check = () => {
			stop();
			stop = () => {};
			this.sync();
			const copy = this.#serving();
			if (copy) stop = hooks.watchSubscriberError(copy.sub, fn);
			const changed = this.route.changed.changed(check);
			const previous = stop;
			stop = () => {
				previous();
				changed();
			};
		};
		check();
		return () => stop();
	}

	bufferedLargest(): track.Position | undefined {
		this.sync();
		const serving = this.#serving();
		return serving && hooks.bufferedLargest(serving.sub);
	}

	/**
	 * The group a {@link tryGroup} with nothing to drop would hand out next, without taking it: the
	 * first any copy offers. A front reading this one as its copy picks across copies by it.
	 */
	peek(ordered: boolean, delivered: Delivered): bigint | undefined {
		this.#delivered = delivered;
		this.sync();
		if (this.#closed) return undefined;
		// As the take does, so a late group on a copy that ran out is offered here too. A serving
		// copy it reopens owes a backfill, which the next take asks.
		this.#reopen();
		this.#reopenFolded(delivered);
		const { next } = this.#first(ordered, delivered);
		if (!next) return undefined;
		return hooks.peekGroup(next.sub, ordered, this.#newest === undefined ? undefined : this.#newest + 1n);
	}

	// Of the copies holding the lowest group, servable if any can serve it, so which copy holds a
	// group never decides whether it can be served.
	lowestHeld(from: bigint, below: bigint, servable: boolean): Held | undefined {
		let lowest: Held | undefined;
		for (const copy of this.#copies) {
			const held = hooks.lowestHeld(copy.sub, from, lowest ? lowest.key + 1n : below, servable);
			if (!held) continue;
			if (held.key === lowest?.key) lowest.servable ||= held.servable;
			else lowest = held;
		}
		return lowest;
	}

	objectLargest(): track.Position | undefined {
		this.sync();
		const serving = this.#serving();
		return serving ? hooks.objectLargest(serving.sub) : this.#ended?.object;
	}

	largestFinished(position: track.Position): boolean {
		this.sync();
		const serving = this.#serving();
		return serving !== undefined && hooks.largestFinished(serving.sub, position);
	}

	peekLargest(): { largest?: track.Position; ended: boolean } {
		this.sync();
		const serving = this.#serving();
		return serving ? hooks.peekLargest(serving.sub) : (this.#ended ?? { ended: false });
	}

	async largest(seen: track.Position | undefined): Promise<track.Position | undefined> {
		for (;;) {
			this.sync();
			const serving = this.#serving();
			const view = serving ? hooks.peekLargest(serving.sub) : this.#ended;
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

	ask(ask: Ask): void {
		this.sync();
		const serving = this.#serving();
		if (serving) hooks.askSubscriber(serving.sub, ask);
	}

	// A request outlives the copy it asked: once that copy closes while the logical track is open,
	// it asks the copy the route serves next, so only the logical track closing rejects it.
	// Mirrors the Rust `Failover`.
	async refresh(signal?: AbortSignal): Promise<void> {
		// How the copy last asked closed: the track's end once no route serves another.
		let failed: unknown;
		for (let asked: number | undefined; ; ) {
			this.sync();
			const closed = this.#shell.closed.peek();
			if (closed !== undefined) throw closed ?? closedBeforeSent(this.route.name);
			signal?.throwIfAborted();
			const generation = this.route.generation;
			if (generation !== asked) {
				asked = generation;
				const serving = this.#serving();
				if (serving) {
					try {
						return await this.#request(serving, signal);
					} catch (err) {
						if (signal?.aborted) throw err;
						failed = err;
						continue;
					}
				}
				// Nothing served when it was called: the next copy's join asks.
				if (failed === undefined) return;
			} else if (this.route.concluded || this.route.refusal) {
				throw failed;
			}
			await new Promise<void>((resolve) => {
				const disposes: Dispose[] = [];
				const wake = () => {
					for (const dispose of disposes) dispose();
					signal?.removeEventListener("abort", wake);
					resolve();
				};
				disposes.push(this.route.changed.changed(wake), this.#shell.closed.changed(wake));
				signal?.addEventListener("abort", wake, { once: true });
			});
		}
	}

	// Ask `serving` for a refresh, withdrawing it once the logical track closes.
	async #request(serving: Copy, signal?: AbortSignal): Promise<void> {
		const asked = new AbortController();
		const abort = () => asked.abort(signal?.reason);
		signal?.addEventListener("abort", abort, { once: true });
		let dispose: Dispose | undefined;
		const ended = new Promise<never>((_, reject) => {
			dispose = this.#shell.closed.changed((closed) => {
				if (closed === undefined) return;
				reject(closed ?? closedBeforeSent(this.route.name));
				asked.abort();
			});
		});
		try {
			return await Promise.race([serving.sub.refresh({ signal: asked.signal }), ended]);
		} finally {
			signal?.removeEventListener("abort", abort);
			dispose?.();
		}
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
		for (const copy of [...this.#copies, ...this.#rerouted]) this.#drop(copy, abort);
		this.#copies = [];
		this.#rerouted = [];
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
	key: bigint,
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
	fetchGroup(name, key, { signal: controller.signal })
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

function closedBeforeSent(name: string): Error {
	return new StreamError(StreamCode.Cancel, { message: `track ${name} closed before the refresh was sent` });
}

/** Why a group a copy closed short ends there: no route holds more of it. */
function standing(): Error {
	return new StreamError(StreamCode.NotFound, { message: "no route holds more of the group" });
}

function dropped(): Error {
	return new StreamError(StreamCode.Cancel, { message: "no route serves the track" });
}

/** Carries a group across route changes; see the module docs. */
class Recover implements GroupRecover {
	#reader: Reader;
	readonly key: bigint;
	/** The copy being read, whose subscription this group keeps alive while it is read. */
	#copy?: Copy;
	/** The route generation when `#copy` was acquired; kept separate because `Reader.sync` updates `copy.generation` in place on switch-back. */
	#generation: number;
	/**
	 * The group being read came from the reader's own fetch, judged under the reader's own terms, or
	 * from looking past a copy's too-old verdict once already: its own too old stands, which bounds
	 * the route to one request more per group.
	 */
	#judged = false;
	/** The recovery under way looks past a copy's too-old verdict. */
	#excusing = false;

	/**
	 * Puts the group on the reader's list of those still read, so the other copies that hold it,
	 * or may yet get it, stay for a failure to continue from.
	 */
	constructor(reader: Reader, group: GroupConsumer, copy: Copy) {
		this.#reader = reader;
		this.key = group.start;
		this.#copy = copy;
		this.#generation = copy.generation;
		copy.lease++;
		reader.out.add(this);
	}

	/**
	 * Release the copy once the group ends. Called only after the group carries this recovery:
	 * reading `closed` before then would settle it on a failure recovery could still continue.
	 */
	watch(group: GroupConsumer): void {
		void hooks.groupClosed(group).then(() => this.close());
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

	wants(cause: RecoverCause): boolean {
		const copy = this.#copy;
		if (!copy) return false;
		const route = this.#reader.route;
		if (cause.kind === "failed" && this.#undelivered(cause.error, copy)) return true;
		// A refusal is terminal: no route continues a short close, which stands.
		if (cause.kind === "closed" && route.refusal) return false;
		// A stall or a short close on a copy whose route failed, or a short close while the front
		// stages the copy to replace it, looks to the route replacing it. Mirrors the Rust
		// `Recover::wants`.
		if (cause.kind === "stalled" && copyFailed(copy)) return true;
		if (cause.kind === "closed" && (routeLost(copy) || route.staged)) return true;
		if (route.concluded) return true;
		if (route.generation !== this.#generation) {
			// A route that returned serves the copy being read again, which a stalled group reuses.
			// A group that closed short (e.g. bare FIN at a Position End cap) will not resume on
			// the same subscription, so recover by fetching the remaining frames.
			if (cause.kind === "closed" || this.#reader.copy(route.generation) !== copy) {
				return true;
			}
			this.#generation = route.generation;
		}
		return false;
	}

	/**
	 * Whether `copy` failed delivering the group rather than judging it, so its own route may be
	 * asked again. Any other failure would repeat on every copy from that route, so only another
	 * route is asked, which bounds a hostile route to one bad answer per route change.
	 */
	#undelivered(failed: Error, copy: Copy): boolean {
		return routeFailed(failed) || copyFailed(copy) || this.#excuses(failed, copy);
	}

	/**
	 * Whether `err` is a too-old verdict from `copy` that is none on the reader: the reader outgrows
	 * the budget it was judged under (see {@link #outgrowsJudging}), and the group being read is no
	 * answer to the reader's own request already. Mirrors the Rust `Recover::excuses`.
	 */
	#excuses(err: Error, copy: Copy): boolean {
		return (
			err instanceof StreamError && err.code === StreamCode.Old && !this.#judged && this.#outgrowsJudging(copy)
		);
	}

	/**
	 * Whether the reader is wider than the narrowest budget `copy`'s upstream asked, which a group
	 * it reset or dropped as too old may have been judged under: that verdict settles a reader only as a stale
	 * drop does (see {@link settles}), and the route still delivers the group to a request under the
	 * reader's own terms. A cache only makes things faster. Mirrors the Rust
	 * `Recover::outgrows_judging`.
	 */
	#outgrowsJudging(copy: Copy): boolean {
		const judged = hooks.judgedUnder(copy.sub);
		if (judged === undefined) return false;
		const run = { start: this.key, end: this.key + 1n, ...staleHole(judged) };
		return !settles(run, this.#reader.within.budget(), this.#reader.coverage());
	}

	async replace(index: number, cause: RecoverCause, signal: AbortSignal): Promise<Replacement> {
		let fetch: Fetch | undefined;
		let kept: GroupConsumer | undefined;
		try {
			const replacement = await this.#replace({ index, cause, signal }, (next) => {
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
		{ index, cause, signal }: { index: number; cause: RecoverCause; signal: AbortSignal },
		started: (fetch: Fetch) => void,
	): Promise<Replacement> {
		const reader = this.#reader;
		const route = reader.route;
		const failed = cause.kind === "failed" ? cause.error : undefined;
		const closed = cause.kind === "closed";
		// The copy being read delivers no more of the group.
		const settled = cause.kind !== "stalled";
		const reading = this.#copy;
		this.#excusing = failed !== undefined && reading !== undefined && this.#excuses(failed, reading);
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
			if (settled && route.refusal) throw route.refusal;
			const generation = route.generation;
			const serving = route.source ? reader.copy(generation) : undefined;
			const waits: Getter<unknown>[] = [route.changed];
			// A failed group continues from any copy still holding it, newest first: its name fixes
			// its content. A stall only moves to the serving copy, so two stalled copies never trade
			// the group back and forth.
			if (failed) {
				const held = reader.holding(this.key, current)[0];
				const cached = held && hooks.cachedGroup(held.sub, this.key, index);
				if (cached) return this.#replacement(cached, held);
				// A verdict would repeat from the route that gave it.
				if (generation === this.#generation && !this.#undelivered(failed, current)) throw failed;
			}
			// An unroutable copy waits for another route rather than asking its own again.
			const withdrawn = failed !== undefined && generation === this.#generation && unroutable(failed);
			if (!serving) {
				// Nothing replaced a copy that closed the group, as when its track parks, so its
				// close stands, unless its route failed: then it waits for a route as a failed
				// copy does, unless none will come.
				if (closed && (!routeLost(current) || route.concluded)) throw standing();
				// Nothing serves: a failed copy waits for a route, unless none will come.
				if (failed && route.concluded) throw failed;
			} else if (closed && generation === this.#generation) {
				// It serves on with no front to replace it, so its close stands, unless its route
				// failed or the front stages the copy to replace it: then that one may hold more.
				if ((!routeLost(current) && !route.staged) || route.concluded) throw standing();
			} else if (generation !== this.#generation || failed) {
				// The serving route gave the group up, which ends it with that route's error however
				// a fetch of the rest would answer. A copy that failed dropped its open groups with
				// it, and a group the route failed is no verdict either.
				const given = hooks.givenUp(serving.sub, this.key);
				if (given.error && !routeFailed(given.error) && !copyFailed(serving)) {
					if (!this.#excuses(given.error, serving)) throw given.error;
					this.#excusing = true;
				}
				waits.push(...given.changed);
				// A newer copy's track caches it: continue from there. On switch-back to the same
				// copy (whose group closed short under a Position End cap), skip its cached copy
				// and fetch the remaining frames from `index`.
				if (!failed && generation !== this.#generation && serving !== current) {
					const cached = hooks.cachedGroup(serving.sub, this.key, index);
					if (cached) return this.#replacement(cached, serving);
				}
				// Finished history may still continue the group above, and so may a fetch already
				// answered below. Otherwise a terminal track verdict ends this wait even while a
				// fetch is still unanswered, and asks for nothing more.
				const closed = serving.sub.closed.peek();
				const verdict = closed instanceof Error && !routeFailed(closed) ? closed : undefined;
				waits.push(serving.sub.closed);
				// Otherwise ask for it, if the route's version can fetch. The subscription may still
				// deliver it first. Without FETCH only a copy continues a failed or closed group, and
				// none holds it, so it ends as the copy left it.
				const fetchGroup = route.view(serving.source).fetchGroup;
				if (!fetchGroup && settled) throw verdict ?? failed ?? standing();
				if (fetchGroup && !verdict && !withdrawn && fetch?.generation !== generation) {
					fetch = startFetch(fetchGroup, route.name, this.key, generation, signal);
					started(fetch);
				}
				// A fetch from an earlier route answers for that route, not this one.
				const asked = fetch?.generation === generation ? fetch : undefined;
				const outcome = asked?.outcome;
				// An answer carries its own expiry; the verdict and the budget only end the wait.
				if (outcome && "group" in outcome) return this.#replacement(outcome.group, serving, true);
				if (verdict) throw verdict;
				// Judged within the reader's window, as a moved group is: the serving copy's subscription
				// starts at the boundary, past groups in the window that supersede this one.
				const stale = hooks.staleAt(serving.sub, this.key, undefined, reader.within);
				if (stale.expired()) throw new StreamError(StreamCode.Old, { message: "group is past the budget" });
				waits.push(...stale.changed, ...(asked && !asked.outcome ? [asked.settled] : []));
				// That route failing too waits for the next one, and a refusal is a verdict only for
				// a copy that delivers no more, once the serving subscription will not deliver the
				// group either.
				if (outcome && settled && !routeFailed(outcome.error) && !copyFailed(serving)) {
					const subscription = serving.sub.subscription.peek();
					const maxDelay = Duration.millis(widest(subscription?.ranges ?? []));
					const land = { start: this.key, end: this.key + 1n, floor: true, grace: grace(maxDelay) };
					if (generation === this.#generation || !hooks.mayLand(serving.sub, land)) throw outcome.error;
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

	#replacement(group: GroupConsumer, copy: Copy, fetched = false): Replacement {
		// Capture the route generation at selection time: `Reader.sync` may rewrite
		// `copy.generation` in place on a switch-back before `adopt` runs.
		const generation = copy.generation;
		return {
			group,
			expiry: hooks.staleAt(copy.sub, this.key, undefined, this.#reader.within),
			adopt: () => {
				// A later switch must keep this copy subscribed while the recovered group still
				// reads it, just like a group handed out from that copy directly. Leased first, so
				// letting go of the copies it leaves never drops it.
				copy.lease++;
				this.#release();
				this.#copy = copy;
				this.#generation = generation;
				this.#judged = fetched || this.#excusing;
				this.#excusing = false;
			},
			discard: () => group.close(),
		};
	}
}

/**
 * A fetched group judged by the publisher's max age alone against a reader's copy of its track on
 * the route it came from, as the Rust relay judges a fetch landed in that copy's cache. It opens
 * no subscription, so a fetch-only relay never pulls the track: with no copy, nothing newer is
 * known here, and the group is never stale.
 */
class FetchExpiry implements Expiry {
	readonly changed: readonly Getter<unknown>[];
	#find: () => Copy | undefined;
	#key: bigint;
	// The copy judging the group. One that closed still judges by what it held, until another opens.
	#judging?: { sub: track.Subscriber; expiry: Expiry; unwatch: Dispose[] };
	// Bumped whenever the judging copy's verdict may change.
	#relay = new Signal(0);
	#closed = false;

	constructor(find: () => Copy | undefined, copied: Getter<unknown>, key: bigint) {
		this.#find = find;
		this.#key = key;
		this.changed = [copied, this.#relay];
	}

	expired(): boolean {
		this.#pick();
		return this.#judging?.expiry.expired() ?? false;
	}

	/** Judge afresh once the group moves to another route: the old route's copy says nothing about it. */
	moved(): void {
		this.#unwatch();
		this.#judging = undefined;
		this.#relay.update((n) => n + 1);
	}

	/** Stop watching the copy, once nobody reads the group. */
	close(): void {
		this.#closed = true;
		this.#unwatch();
	}

	#pick(): void {
		const found = this.#find();
		if (!found || found.sub === this.#judging?.sub) return;
		this.#unwatch();
		const expiry = hooks.staleAt(found.sub, this.#key, true);
		const bump = () => this.#relay.update((n) => n + 1);
		const unwatch = this.#closed ? [] : expiry.changed.map((changed) => changed.subscribe(bump));
		this.#judging = { sub: found.sub, expiry, unwatch };
	}

	#unwatch(): void {
		for (const dispose of this.#judging?.unwatch ?? []) dispose();
		if (this.#judging) this.#judging.unwatch = [];
	}
}

/** A logical track: its name, and whether it is the metadata copy. Mirrors the Rust `track::Key`. */
type TrackKey = { name: string; meta: boolean };

/**
 * One broadcast served through one route at a time; see the module docs.
 *
 * Its {@link consumer} is the broadcast handed out: every track read through it follows the
 * front's route. The origin calls {@link serve} as routes of the same identity take over, and
 * {@link conclude} once none will: readers then follow the last copy to its end.
 */
export class Front {
	/** The logical broadcast; closing its last handle concludes the front. */
	readonly consumer: broadcast.Consumer;
	#source?: Source;
	#routes = new Map<string, Route>();
	/** The metadata copies' routes, kept apart so no payload reader is ever handed one. */
	#metaRoutes = new Map<string, Route>();
	/**
	 * Each track's properties as first committed, and the {@link Source.id} of the source that said
	 * so: every later copy must match. Kept for the front's lifetime, so it holds no route.
	 */
	#info = new Map<string, { info: track.Info; source: number }>();
	/** Bumped whenever the source changes, so an answer from a replaced one is dropped. */
	#changed = new Signal(0);
	/** Bumped whenever a reader subscribes a copy, which may judge a fetched group; see {@link FetchExpiry}. */
	#copied = new Signal(0);
	/** Aborted once the current source is replaced, retiring what was asked of it. */
	#retire = new AbortController();
	#concluded = false;
	/** The tracks each source was checked against the recorded properties for; see #verify. */
	#verified = new WeakMap<Source, Set<string>>();

	constructor() {
		this.consumer = makeFrontConsumer(this);
		void this.consumer.closed.then(() => this.conclude());
		void this.#list(wireOf(this.consumer).lister());
	}

	/**
	 * Answer listings of the front's broadcast with its serving source's listing, while anyone
	 * lists it. A source of the same identity lists the same tracks, so a takeover shows readers
	 * only what differs, once the new source's listing synced.
	 */
	async #list(lister: Lister): Promise<void> {
		const closed = this.consumer.closed;
		// What the front lists, by kind and name, the source being read, and its pending next event.
		type Named = { name: string; datagrams?: boolean };
		let listing:
			| {
					listed: Map<string, Named>;
					/** Tracks an earlier source listed that the current one has yet to list. */
					stale: Map<string, Named>;
					source?: Source;
					reader?: Tracks;
					next?: Promise<{ event?: TracksEvent; err?: Error }>;
			  }
			| undefined;
		const wait = () => race([Signal.race(lister.wanted, this.#changed), closed]);
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
			listed: Map<string, Named>,
			stale: Map<string, Named>,
			event: { kind: "removed" | "finished"; name: string; datagrams?: boolean },
		) => {
			const key = keyOf(event.name, event.datagrams);
			stale.delete(key);
			if (!listed.delete(key)) return;
			if (event.kind === "finished") lister.finish(event.name, event.datagrams);
			else lister.remove(event.name, event.datagrams);
		};
		try {
			for (;;) {
				if (closed.peek() !== undefined) return;
				if (!lister.wanted.peek()) {
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
				listing ??= { listed: new Map(), stale: new Map() };
				if (listing.source !== this.#source) {
					listing.reader?.close();
					listing.source = this.#source;
					listing.stale = new Map(listing.listed);
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
				// One flat race: nested, the wait's registrations stay pending whenever the event wins.
				// What a change or the close settles with is no event, so the loop looks again.
				const result = await race([listing.next, Signal.race(lister.wanted, this.#changed), closed]);
				if (typeof result !== "object" || result === null) continue;
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
					const key = keyOf(event.name, event.datagrams);
					if (!listing.stale.delete(key) && !listing.listed.has(key)) {
						listing.listed.set(key, { name: event.name, datagrams: event.datagrams });
						lister.add(event.name, event);
					}
				} else if (event.kind === "removed" || event.kind === "finished") {
					unlist(listing.listed, listing.stale, event);
				} else {
					// What the new source still lacks at its SYNCED is gone.
					for (const [key, { name, datagrams }] of listing.stale) {
						listing.listed.delete(key);
						lister.remove(name, datagrams);
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
	serve(from: broadcast.Consumer, local = false): void {
		if (this.#concluded || this.#source?.from === from) return;
		const source = new Source(from, local);
		source.retain();
		const previous = this.#source;
		this.#source = source;
		this.#retired();
		for (const route of this.#everyRoute()) this.#splice(route, source);
		previous?.release();
	}

	/**
	 * The table moved for the path while the same route still wins, with no takeover pending: a
	 * read stranded on a lost route asks the serving source again, the evidence it waited for.
	 * Mirrors the Rust front's `unstrand`.
	 */
	moved(): void {
		const current = this.#source;
		if (this.#concluded || !current) return;
		for (const route of this.#everyRoute()) {
			if (!route.unstrand()) continue;
			// Read from the serving source: subscribe afresh. Otherwise the read is on a copy the
			// serving source replaced, which takes over again as any handover does.
			if (route.source === current) route.resubscribe();
			else this.#splice(route, current);
		}
	}

	/** Between routes: readers keep what they have and wait for the next one. */
	park(): void {
		if (this.#concluded || !this.#source) return;
		const previous = this.#source;
		this.#source = undefined;
		this.#retired();
		for (const route of this.#everyRoute()) route.park();
		previous.release();
	}

	/** No route will replace the serving one: readers follow it to its end. Idempotent. */
	conclude(): void {
		if (this.#concluded) return;
		this.#concluded = true;
		for (const route of this.#everyRoute()) route.conclude();
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

	// Splice `source`'s copy of a track in, once it claims the same content as the copies before.
	#splice(route: Route, source: Source): void {
		const recorded = this.#info.get(route.name);
		const previous = route.served;
		// The first copy of a track has nothing to match, and the source that said what it is
		// matches itself.
		if ((!recorded && !previous) || recorded?.source === source.id) {
			route.serve(source);
			return;
		}
		// The replaced copy keeps feeding the track while the new one answers. One that has not
		// answered yet is asked too, so a swap racing its first answer is still checked.
		source.retain();
		previous?.retain();
		route.stage(source);
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
					route.unstage(source);
					if (this.#source !== source) return;
					// The copy it replaces never answered, so this one says what the track is.
					if (!expected) this.#record(route.name, info, source);
					// A copy claiming the same content with different metadata is refused rather
					// than reinterpreted.
					if (!expected || compatible(expected, info)) route.serve(source);
					else route.refuse(new Incompatible(route.name));
				},
				(err: unknown) => {
					route.unstage(source);
					if (this.#source !== source) return;
					const error = err instanceof Error ? err : new Error(String(err));
					// A route that lost the content leaves the replaced copy feeding the read, which asks
					// again once the table moves, as the Rust front's refusal does. A route on its way
					// out is no verdict: the next one is asked.
					if (strands(error, source)) route.strand(source);
					else if (!routeFailed(error)) route.refuse(error);
				},
			)
			.finally(() => {
				source.release();
				previous?.release();
			});
	}

	// Every logical track's route, payload and metadata copies alike.
	*#everyRoute(): Iterable<Route> {
		yield* this.#routes.values();
		yield* this.#metaRoutes.values();
	}

	#route(name: string, meta: boolean): Route {
		const routes = meta ? this.#metaRoutes : this.#routes;
		let route = routes.get(name);
		if (route) return route;
		route = new Route(name, meta);
		const created = route;
		created.idle = () => {
			if (routes.get(name) === created) routes.delete(name);
		};
		created.copied = () => this.#copied.update((n) => n + 1);
		routes.set(name, created);
		// A track whose properties another source already gave is checked like any replacement.
		if (this.#concluded) created.conclude();
		else if (this.#source) this.#splice(created, this.#source);
		return created;
	}

	// Check `source` claims the same properties for `name` as recorded, once per source: a fetch
	// never reads a copy a subscription would refuse.
	async #verify(name: string, source: Source): Promise<void> {
		const recorded = this.#info.get(name);
		if (recorded?.source === source.id) return;
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
		if (!this.#info.has(name)) this.#info.set(name, { info, source: source.id });
	}

	/** Subscribe the logical track `name`, or its metadata copy when `meta`. */
	subscribe(name: string, options?: track.Subscription, meta = false): track.Subscriber {
		if (this.consumer.closed.peek() !== undefined) throw new Error("broadcast is closed");
		const route = this.#route(name, meta);
		return hooks.resumeSubscriber(
			name,
			options,
			(shell) => new Reader(route, shell, (info, source) => this.#record(name, info, source)),
		);
	}

	/** Join the serving source's live datagram track; it holds no cached route copies. */
	datagrams(name: string): import("./datagram.ts").Consumer {
		if (!this.#source) throw new NotFound(`datagram track ${name}`);
		return this.#source.wire.datagrams(name);
	}

	/** Join an already present datagram track without opening a request. */
	liveDatagrams(name: string): import("./datagram.ts").Consumer | undefined {
		return this.#source?.wire.liveDatagrams(name);
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

	largest(name: string, meta = false): track.Position | undefined {
		const source = this.#source;
		return source && (meta ? source.meta : source.wire).largest(name);
	}

	async fetchGroup(
		name: string,
		key: bigint,
		options?: track.FetchGroupOptions,
		meta = false,
	): Promise<GroupConsumer> {
		const target = { name, meta };
		const fetched = await this.#fetch(target, key, options);
		let current = fetched.source;
		// A group fetched over a wire carries no expiry of its own, unlike one a local track
		// answers: the publisher's max age judges it against a copy of the route it came from.
		const judge = new FetchExpiry(() => this.#copyOn(target, current), this.#copied, key);
		const expiry = (group: GroupConsumer) => hooks.groupExpiry(group) ?? judge;
		if (!hooks.groupExpiry(fetched.group)) hooks.expireGroup(fetched.group, judge);
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
			wants: (cause) => {
				if (cause.kind === "failed" && undelivered(cause.error)) return true;
				return this.#source !== undefined && this.#source !== current;
			},
			replace: async (_index, cause, signal) => {
				// A fetch that failed delivering may ask its own route again; a stalled or closed one,
				// one a verdict ended, or one whose route withdrew only moves to another. A withdrawal
				// keeps its cause once none will come.
				const failed = cause.kind === "failed" ? cause.error : undefined;
				const withdrawn = failed && unroutable(failed) ? failed : undefined;
				const avoid = failed && undelivered(failed) && !withdrawn ? undefined : current;
				const next = await this.#fetch(target, key, { ...options, signal }, avoid, withdrawn);
				// The fetch's hold on its route passes to the group on adoption, or goes with the discard.
				return {
					group: next.group,
					expiry: expiry(next.group),
					adopt: () => {
						current.release();
						current = next.source;
						judge.moved();
						// Its fetch may hold its subscription until its reader lets go, and the
						// reader is now `fetched.group`.
						void hooks.groupReleased(fetched.group).then(() => next.group.close());
					},
					discard: () => {
						next.group.close();
						next.source.release();
					},
				};
			},
		});
		// Only once the group carries its recovery: reading `closed` or `done` before then would
		// settle it on a failure recovery could still continue, releasing the route, or ending
		// the budget's watch, while it is read.
		void hooks.groupClosed(fetched.group).then(() => current.release());
		// The budget is watched for as long as the group is read, or until released while a held
		// reply awaits its acknowledgement, as the fetch's own subscription is.
		void (
			(options as broadcast.FetchFrom | undefined)?.held
				? hooks.groupReleased(fetched.group)
				: hooks.groupDone(fetched.group)
		).then(() => judge.close());
		return fetched.group;
	}

	// The newest open copy of `target` a reader holds of `source`'s broadcast, if any.
	#copyOn(target: TrackKey, source: Source): Copy | undefined {
		const routes = target.meta ? this.#metaRoutes : this.#routes;
		for (const reader of routes.get(target.name)?.readers ?? []) {
			const copy = reader.copyOn(source);
			if (copy) return copy;
		}
		return undefined;
	}

	// Fetch a group from whichever route serves the track, asking the next one when a route
	// fails, and never `avoid`. The source comes back retained for as long as the group is read.
	// Once no route will come, it ends with `ended`, or as not found.
	async #fetch(
		target: TrackKey,
		key: bigint,
		options: track.FetchGroupOptions | undefined,
		avoid?: Source,
		ended?: Error,
	): Promise<{ group: GroupConsumer; source: Source }> {
		const { name, meta } = target;
		for (;;) {
			const source = this.#source;
			if (!source || source === avoid) {
				if (this.#concluded) throw ended ?? new NotFound(`group ${key}`);
				await this.#next(options?.signal);
				continue;
			}
			source.retain();
			// Abandoned once another route takes over, and asked of that one instead.
			const retired = this.#retire.signal;
			const signal = options?.signal ? AbortSignal.any([options.signal, retired]) : retired;
			try {
				// Asked alongside the check that the source claims the track's recorded properties,
				// rather than a round trip after it: only handing the group out waits for the check.
				const fetchGroup = (meta ? source.meta : source.wire).fetchGroup;
				const ahead = new AbortController();
				const fetching = fetchGroup?.(name, key, {
					...options,
					signal: AbortSignal.any([signal, ahead.signal]),
				});
				fetching?.catch(() => {});
				try {
					await until(this.#verify(name, source), signal);
				} catch (err) {
					ahead.abort(err);
					void fetching?.then(
						(group) => group.close(),
						() => {},
					);
					throw err;
				}
				if (!fetching) throw new Error("fetch group is not supported by this session's version");
				const group = await fetching;
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
		const routed: BroadcastWire = {
			...view,
			datagrams: (name) => front.datagrams(name),
			liveDatagrams: (name) => front.liveDatagrams(name),
			tracks: view.tracks,
			lister: view.lister,
			subscribe: (name, options) => front.subscribe(name, options),
			resolveTrackInfo: (name, hold) => front.resolveTrackInfo(name, hold),
			largest: (name) => front.largest(name),
			fetchGroup: (name, key, options) => front.fetchGroup(name, key, options),
		};
		// Each route's source decides which copy a metadata reader gets; the front keeps the two
		// apart either way. Both copies claim the same properties, so info is asked once.
		const meta: BroadcastWire = {
			...routed,
			subscribe: (name, options) => front.subscribe(name, options, true),
			largest: (name) => front.largest(name, true),
			fetchGroup: (name, key, options) => front.fetchGroup(name, key, options, true),
		};
		registerWire(this, { ...routed, meta: () => meta });
	}

	static {
		makeFrontConsumer = (front) => new FrontConsumer(front);
	}

	// Preserve the subclass, and the front behind it, when a handle is shared.
	override clone(): FrontConsumer {
		return new FrontConsumer(this.#front, this.shareState());
	}
}

/** The runs of `held` inside `[start, end)`. */
function runsIn(held: Delivered, start: bigint, end: bigint): Run[] {
	const runs: Run[] = [];
	let at = start;
	for (const gap of held.gaps(start, end)) {
		if (gap.start > at) runs.push({ start: at, end: gap.start });
		at = gap.end;
	}
	if (at < end) runs.push({ start: at, end });
	return runs;
}

/**
 * Whether `copy` is of `serving`'s route, did not fail, and was asked for group `key`. Mirrors the
 * Rust `Copy::asked_for`.
 */
function copyAskedFor(copy: Copy, serving: Copy, key: bigint): boolean {
	return (
		!(copy.done instanceof Error) &&
		copy.source.from.closed === serving.source.from.closed &&
		(copy.floor ?? 0n) <= key &&
		key < (copyEnd(copy) ?? UNBOUNDED)
	);
}

/** The parts of `[start, end)` under `copy`'s bound that its publisher skipped. */
function copySkips(copy: Copy, start: bigint, end: bigint): Run[] {
	const bound = copyEnd(copy);
	const to = bound === undefined ? end : min(end, bound);
	return start < to ? hooks.skippedIn(copy.sub, start, to) : [];
}
