/**
 * Serving the groups a track's readers miss: the on-demand request surface. Mirrors the Rust
 * `track::Requests` and `track::Fill` (`rs/moq-net/src/model/track/fill.rs`).
 *
 * A reader misses a group its ranges name that the track no longer caches, or never did, below
 * the newest group the producer wrote: nothing above it is a miss, since it arrives live. While a
 * {@link Requests} handle is open, each run of misses queues a {@link Fill} for it to serve, and
 * readers missing the same groups join the one fill rather than asking twice. With none open, a
 * miss is given up as it always was.
 *
 * @module
 */
import { Duration } from "@moq/clock";
import { type GetPromise, type Getter, Signal } from "@moq/signals";
import { NotFound, StreamCode, toStreamCode } from "./error.ts";
import type { Producer as GroupProducer } from "./group.ts";
import { cutRun, insertRun, NO_MAX_DELAY, runGaps, runsFrom } from "./internal.ts";
import type { Info, Order } from "./track.ts";
import { checkEnd, checkSequence, max, min } from "./util/sequence.ts";

// A half-open run of group sequences, as `internal.ts` has it, kept local so the published
// declarations never reach the package internals.
type Run = { start: bigint; end: bigint };

/**
 * What the fills need from the track they serve.
 *
 * @internal
 */
export interface FillTrack {
	/** The track's name. */
	readonly name: string;
	/** Insert group `sequence` into the cache, committing `info` first if the track has none. */
	accept(sequence: bigint, info: Info | undefined): GroupProducer;
	/**
	 * Record `[start, end)` as never to be delivered, with `error`. With `stale`, judged too old
	 * under that budget in milliseconds: a verdict only on readers no more tolerant.
	 */
	drop(start: bigint, end: bigint, hole: { error: Error; stale?: number }): void;
	/** Whether the track still caches group `sequence`, not failed. */
	holds(sequence: bigint): boolean;
	/** Whether the track closed, so nothing more can be served. */
	closed(): boolean;
	/** Settles once the track closes. */
	readonly released: GetPromise<unknown>;
}

/**
 * A reader waiting on fills.
 *
 * @internal
 */
export interface Asker {
	/** The parts of `[start, end)` it still reads. */
	wants(start: bigint, end: bigint): Run[];
	/** Its delivery priority. */
	priority(): number;
	/** Its age budget in milliseconds: `Infinity` for none, so the publisher's alone bounds it. */
	maxDelay(): number;
	/** Which end it reads first. */
	order(): Order;
	/**
	 * Stop waiting for `[start, end)`, which a fill refused with `error`, without asking again.
	 * With `stale`, the source judged them too old under that budget in milliseconds: a verdict
	 * on this reader only while its own budget is no wider, so a widening asks again.
	 */
	refuse(start: bigint, end: bigint, error: Error, stale?: number): void;
}

// The fill operations a handle runs against the state it came from; assigned in its static block.
let fillOps: {
	accept(state: FillState, pending: Pending, sequence: bigint, info: Info | undefined): GroupProducer;
	drop(state: FillState, pending: Pending, start: bigint, end: bigint, hole?: { error: Error; stale?: number }): void;
	refuse(state: FillState, pending: Pending, start: bigint, end: bigint, error: Error, stale?: number): void;
	settle(state: FillState, pending: Pending): void;
	name(state: FillState): string;
};

// A part of a fill's run it still owes.
type Live = Run & { pending: Pending };

// One fill: a run of groups some readers miss, until each is answered or nobody wants it.
class Pending {
	readonly start: bigint;
	readonly end: bigint;
	readonly askers = new Set<Asker>();
	// The groups answered: accepted, delivered live, dropped, or refused.
	readonly resolved: Run[] = [];
	// The groups no reader read any more at some point: withdrawn for good, so a handler that
	// stopped serving one is never asked for it again. A reader widening back asks a new fill.
	readonly withdrawn: Run[] = [];
	// Still in the queue, not yet handed to a handler.
	queued = true;
	// Withdrawn or settled: owes nothing more.
	done = false;
	// Bumped on every change to what the fill owes, for its handler to wake on.
	readonly revision = new Signal(0);

	constructor(start: bigint, end: bigint) {
		this.start = start;
		this.end = end;
	}

	bump(): void {
		this.revision.update((revision) => revision + 1);
	}

	// The parts of `[start, end)` within the fill still unanswered and not withdrawn.
	unresolved(start = this.start, end = this.end): Run[] {
		if (this.done) return [];
		return runGaps(this.resolved, max(start, this.start), min(end, this.end)).flatMap((gap) =>
			runGaps(this.withdrawn, gap.start, gap.end),
		);
	}

	// The parts of `[start, end)` still unanswered that a reader still reads.
	owed(start = this.start, end = this.end): Run[] {
		const owed: Run[] = [];
		for (const gap of this.unresolved(start, end)) {
			for (const asker of this.askers) {
				for (const run of asker.wants(gap.start, gap.end)) insertRun(owed, run.start, run.end);
			}
		}
		return owed;
	}
}

/**
 * The fills of one track: what its readers miss, queued for its {@link Requests} handles. A
 * producer owns one.
 *
 * @internal
 */
export class FillState {
	readonly #track: FillTrack;
	#handlers = 0;
	#queue: Pending[] = [];
	readonly #queued = new Signal(0);
	readonly #pending = new Set<Pending>();
	// What each fill still owes, by sequence: its unanswered, unwithdrawn parts, which no two fills
	// share, so a reader's ask or an arrival costs the fills it touches, never every fill out.
	#live: Live[] = [];
	// Bumped as each handler opens, so readers parked on a miss nobody could take ask again.
	readonly #opened = new Signal(0);

	constructor(track: FillTrack) {
		this.#track = track;
		void track.released.then(() => this.#release());
	}

	/** Whether a handler is open to serve a miss now. */
	get serving(): boolean {
		return this.#handlers > 0 && !this.#track.closed();
	}

	/** Whether a fill is out that a reader could join, handler or not. */
	get active(): boolean {
		return this.#pending.size > 0;
	}

	/**
	 * Ask for `[start, end)` for `asker`: join the fills already bringing part of it, and queue the
	 * rest while a handler is open, as the Rust `FillState::join` does. Returns the parts now
	 * asked, in order.
	 */
	ask(start: bigint, end: bigint, asker: Asker): Run[] {
		if (start >= end || this.#track.closed()) return [];
		const asked: Run[] = [];
		for (const live of this.#overlapping(start, end)) {
			if (!live.pending.askers.has(asker)) {
				live.pending.askers.add(asker);
				live.pending.bump();
			}
			insertRun(asked, max(live.start, start), min(live.end, end));
		}
		if (this.#handlers === 0) return asked;
		const missing = runGaps(asked, start, end);
		for (const gap of missing) {
			const pending = new Pending(gap.start, gap.end);
			pending.askers.add(asker);
			this.#pending.add(pending);
			this.#queue.push(pending);
			this.#live.splice(runsFrom(this.#live, gap.start), 0, { start: gap.start, end: gap.end, pending });
			insertRun(asked, gap.start, gap.end);
		}
		if (missing.length > 0) this.#queued.update((revision) => revision + 1);
		return asked;
	}

	/**
	 * The parts of `[start, end)` a fill `asker` joined still owes it, merged and in order: only what it still
	 * reads, as the Rust `Join::held` holds, so a wider fill shared with other readers owes it
	 * nothing outside its own ranges.
	 */
	*covered(start: bigint, end: bigint, asker: Asker): Generator<Run> {
		// Walked lazily, so a caller wanting only the first run costs only the fills up to it.
		let run: Run | undefined;
		for (let i = runsFrom(this.#live, start); i < this.#live.length && this.#live[i].start < end; i++) {
			const live = this.#live[i];
			if (!live.pending.askers.has(asker)) continue;
			for (const part of asker.wants(max(live.start, start), min(live.end, end))) {
				if (run && run.end === part.start) run.end = part.end;
				else {
					if (run) yield run;
					run = { ...part };
				}
			}
		}
		if (run) yield run;
	}

	/** `asker` left: the fills it alone waited on are withdrawn. */
	leave(asker: Asker): void {
		for (const pending of this.#pending) {
			if (!pending.askers.delete(asker)) continue;
			this.#judge(pending);
		}
	}

	/**
	 * What `asker` reads in `[start, end)` changed: a fill nobody reads any more is withdrawn. Only
	 * the fills still owing a group there are judged again, so a change costs what it touches.
	 */
	moved(asker: Asker, start = 0n, end?: bigint): void {
		const judged = new Set<Pending>();
		for (const live of end === undefined
			? this.#live.slice(runsFrom(this.#live, start))
			: this.#overlapping(start, end)) {
			if (live.pending.askers.has(asker)) judged.add(live.pending);
		}
		for (const pending of judged) this.#judge(pending);
	}

	/** Group `sequence` landed in the cache, through a fill or live: it is answered. */
	arrived(sequence: bigint): void {
		const [live] = this.#overlapping(sequence, sequence + 1n);
		if (live) this.#resolve(live.pending, sequence, sequence + 1n);
	}

	/** `[start, end)` was given up, by a fill or otherwise: nobody waits for it from a fill. */
	dropped(start: bigint, end: bigint): void {
		const owing = new Set(this.#overlapping(start, end).map((live) => live.pending));
		for (const pending of owing) this.#resolve(pending, start, end);
	}

	// The live parts overlapping `[start, end)`, lowest first.
	#overlapping(start: bigint, end: bigint): Live[] {
		const found: Live[] = [];
		for (let i = runsFrom(this.#live, start); i < this.#live.length && this.#live[i].start < end; i++)
			found.push(this.#live[i]);
		return found;
	}

	// Take `runs` of one fill out of the live parts, once it no longer owes them.
	#settled(runs: readonly Run[]): void {
		for (const run of runs) cutRun(this.#live, run.start, run.end);
	}

	/** Bumped as each handler opens: a reader parked on a miss watches it to ask again. */
	get opened(): Getter<number> {
		return this.#opened;
	}

	/** Open a handler, waking the readers parked on a miss to ask it. */
	addHandler(): void {
		this.#handlers++;
		this.#opened.update((opened) => opened + 1);
	}

	/** Close a handler. The last one leaving names every queued miss dropped, as with none open. */
	removeHandler(): void {
		this.#handlers--;
		if (this.#handlers > 0) return;
		for (const pending of this.#queue.splice(0)) this.#settle(pending);
	}

	/**
	 * The next queued fill, or `undefined` once the track closes with none queued or `closed`, the
	 * asking handle's, is set.
	 */
	async next(closed: Signal<boolean>): Promise<Pending | undefined> {
		for (;;) {
			if (closed.peek()) return undefined;
			const pending = this.#queue.shift();
			if (pending) {
				pending.queued = false;
				return pending;
			}
			if (this.#track.closed()) return undefined;
			await Signal.race(this.#queued, this.#track.released, closed);
		}
	}

	// Withdraw what no reader reads any more for good, and `pending` once that is all of it.
	#judge(pending: Pending): void {
		if (pending.done) return;
		const owed = pending.owed();
		const withdrawn: Run[] = [];
		for (const gap of pending.unresolved()) {
			for (const run of runGaps(owed, gap.start, gap.end)) {
				insertRun(pending.withdrawn, run.start, run.end);
				withdrawn.push(run);
			}
		}
		this.#settled(withdrawn);
		if (owed.length > 0) {
			pending.bump();
			return;
		}
		this.#finish(pending);
	}

	// Record `[start, end)` of `pending` answered.
	#resolve(pending: Pending, start: bigint, end: bigint): void {
		const unresolved = pending.unresolved(start, end);
		if (unresolved.length === 0) return;
		for (const run of unresolved) insertRun(pending.resolved, run.start, run.end);
		this.#settled(unresolved);
		if (pending.unresolved().length === 0) this.#finish(pending);
		else pending.bump();
	}

	// Answer what `pending` still owes as dropped, then forget it.
	#settle(pending: Pending): void {
		for (const run of pending.owed())
			this.#track.drop(run.start, run.end, { error: new NotFound(`group ${run.start}`) });
		this.#finish(pending);
	}

	// Forget `pending`: it owes nothing more.
	#finish(pending: Pending): void {
		if (pending.done) return;
		this.#settled(pending.unresolved());
		pending.done = true;
		this.#pending.delete(pending);
		if (pending.queued) this.#queue.splice(this.#queue.indexOf(pending), 1);
		pending.bump();
	}

	#release(): void {
		this.#queue = [];
		for (const pending of this.#pending) this.#finish(pending);
		this.#live = [];
		this.#queued.update((revision) => revision + 1);
	}

	static {
		fillOps = {
			accept: (state, pending, sequence, info) => {
				if (sequence < pending.start || sequence >= pending.end) {
					throw new RangeError(`group ${sequence} is outside the fill ${pending.start}..${pending.end}`);
				}
				if (pending.resolved.some((run) => run.start <= sequence && sequence < run.end)) {
					throw new Error(`group ${sequence} was already answered`);
				}
				if (state.#track.holds(sequence)) throw new Error(`duplicate group: sequence=${sequence}`);
				// Landing in the cache answers it, as `arrived` records.
				return state.#track.accept(sequence, info);
			},
			drop: (state, pending, start, end, hole) => {
				for (const run of pending.owed(start, end)) {
					state.#track.drop(run.start, run.end, hole ?? { error: new NotFound(`group ${run.start}`) });
				}
				// A stale verdict settles only readers that read every group above it, so the readers
				// waiting now are refused as well, as the Rust `Inner::drop_groups` does. It binds every
				// reader, so the refusal carries no budget a wider one could see past.
				if (hole?.stale !== undefined) fillOps.refuse(state, pending, start, end, hole.error);
				else state.#resolve(pending, start, end);
			},
			refuse: (state, pending, start, end, error, stale) => {
				for (const gap of pending.unresolved(start, end)) {
					for (const asker of pending.askers) {
						for (const run of asker.wants(gap.start, gap.end))
							asker.refuse(run.start, run.end, error, stale);
					}
				}
				state.#resolve(pending, start, end);
			},
			settle: (state, pending) => state.#settle(pending),
			name: (state) => state.#track.name,
		};
	}
}

/** The still-wanted runs of a fill, in ascending order. @internal */
export let wantedRuns: (fill: Fill) => Run[];
/**
 * Wait for a change to the groups or readers of a fill, which a lost {@link race} releases.
 * @internal
 */
export let fillChanged: (fill: Fill) => GetPromise<unknown>;
/**
 * The budget in milliseconds an OLD answer was judged under, and whether that is the publisher's
 * max age, which binds every reader, rather than the request's own budget.
 *
 * @internal
 */
export interface Judged {
	budget: number;
	everyone: boolean;
}

/**
 * Answer `[start, end)` of `fill` with its source's refusal of a request for them. An OLD one
 * says every older group of the fill is stale too, since a timed track's starts never rewind:
 * judged under the request's own budget, or a max age not known here (`judged` undefined), it is
 * refused to the readers waiting now, a verdict only on those no more tolerant, so a wider reader
 * asks again; under the publisher's max age it binds every reader, so the track records it as a
 * stale verdict under that age, which new terms or the drop set's expiry window forget. Any other
 * refusal answers only `[start, end)`, for now. Mirrors the Rust `Fill::reject_groups`.
 * @internal
 */
export let rejectGroups: (fill: Fill, start: bigint, end: bigint, error: Error, judged: Judged | undefined) => void;

// Constructs the public handles from within this module; assigned in their static blocks.
let makeFill: (state: FillState, pending: Pending) => Fill;
let makeRequests: (state: FillState) => Requests;

/**
 * A {@link Requests} handle on `state`, for a producer's `requests()`.
 *
 * @internal
 */
export function requestsOf(state: FillState): Requests {
	return makeRequests(state);
}

/**
 * Serves the groups a track's readers miss: from {@link Producer.requests}. Mirrors the Rust
 * `track::Requests`.
 *
 * Most tracks never serve old content, so this lives on its own handle rather than the producer:
 * a publisher backed by storage opens one to serve groups its live cache no longer holds, or
 * never held. While at least one is open, a reader missing a run of groups queues a {@link Fill}
 * here; with none open, the miss is given up. {@link close} it once done.
 *
 * @public
 */
export class Requests {
	#state: FillState;
	#closed = new Signal(false);

	private constructor(state: FillState) {
		this.#state = state;
		state.addHandler();
	}

	static {
		makeRequests = (state) => new Requests(state);
	}

	/** The track's name, unique within its broadcast. */
	get name(): string {
		return fillOps.name(this.#state);
	}

	/**
	 * Wait until readers miss a run of groups, returning the {@link Fill} to serve. Resolves
	 * `undefined` once the track closes with nothing queued, or this handle closes, even while
	 * it waits.
	 */
	async requestedFill(): Promise<Fill | undefined> {
		const pending = await this.#state.next(this.#closed);
		if (!pending) return undefined;
		return makeFill(this.#state, pending);
	}

	/**
	 * Stop serving. The last handle closing names every miss still queued dropped, so its
	 * readers stop waiting; a fill already handed out keeps its own handle. Idempotent.
	 */
	close(): void {
		if (this.#closed.peek()) return;
		this.#closed.set(true);
		this.#state.removeHandler();
	}

	[Symbol.dispose](): void {
		this.close();
	}
}

/**
 * A run of groups readers miss, from {@link Requests.requestedFill}: `start` up to the exclusive
 * `end`. Mirrors the Rust `track::Fill`.
 *
 * Its {@link priority}, {@link maxDelay}, and {@link order} follow its readers as they change, so
 * read them as each group is served. Call {@link accept} for each group the source has, writing
 * its frames into the returned producer, and {@link dropGroups} for those it does not. Readers
 * that leave, or narrow their ranges past a group, stop wanting it: serve {@link wanted} until it
 * is `undefined`, which {@link wantedChanged} waits on. {@link close} the fill once done; closing
 * names whatever is still wanted dropped rather than leaving its readers waiting.
 *
 * Groups keep their sequence, so an accepted group should hold the frames, payloads, and
 * boundaries it had when it was first published: a fill replays the track, never a new version.
 *
 * @public
 */
export class Fill {
	/** The first group to fill. */
	readonly start: bigint;
	/** The exclusive end group. */
	readonly end: bigint;

	#state: FillState;
	#pending: Pending;
	// The value `wantedChanged` last returned, or unset before it returned any.
	#prev?: { wanted: bigint | undefined };

	private constructor(state: FillState, pending: Pending) {
		this.#state = state;
		this.#pending = pending;
		this.start = pending.start;
		this.end = pending.end;
	}

	static {
		makeFill = (state, pending) => new Fill(state, pending);
		wantedRuns = (fill) => fill.#pending.owed();
		fillChanged = (fill) => Signal.race(fill.#pending.revision);
		rejectGroups = (fill, start, end, error, judged) => {
			if (toStreamCode(error) !== StreamCode.Old) {
				fill.refuseGroups(start, end, error);
			} else if (judged?.everyone) {
				fillOps.drop(fill.#state, fill.#pending, fill.start, end, { error, stale: judged.budget });
			} else {
				fillOps.refuse(fill.#state, fill.#pending, fill.start, end, error, judged?.budget);
			}
		};
	}

	/** The most urgent delivery priority among the readers waiting on it now. */
	get priority(): number {
		let priority: number | undefined;
		for (const asker of this.#pending.askers) priority = Math.max(priority ?? asker.priority(), asker.priority());
		return priority ?? 0;
	}

	/**
	 * The most tolerant age budget among the readers waiting on it now, or `null` for none: the
	 * publisher's {@link Info.maxAge} alone bounds it. Each reader still enforces its own.
	 */
	get maxDelay(): Duration | null {
		let max = 0;
		for (const asker of this.#pending.askers) max = Math.max(max, asker.maxDelay());
		return max < NO_MAX_DELAY ? Duration.millis(max) : null;
	}

	/** Which end to fill first now: `"descending"`, newest first, if any reader asks it. */
	get order(): Order {
		for (const asker of this.#pending.askers) if (asker.order() === "descending") return "descending";
		return this.#pending.askers.size > 0 ? "ascending" : "descending";
	}

	/** Whether a reader still waits for group `sequence` from this fill. Once false, it stays false. */
	isWanted(sequence: bigint): boolean {
		checkSequence(sequence);
		return this.#pending.owed(sequence, sequence + 1n).length > 0;
	}

	/**
	 * The next group still owed in the fill's {@link order}: a reader still reads it, and it was
	 * neither answered nor delivered live since it was asked. `undefined` once none is, so stop
	 * serving the fill.
	 */
	wanted(): bigint | undefined {
		const owed = this.#pending.owed();
		if (owed.length === 0) return undefined;
		return this.order === "ascending" ? owed[0].start : (owed.at(-1) as Run).end - 1n;
	}

	/**
	 * {@link wanted} once it differs from what this handle last returned: at once on the first
	 * call, then on each change. So `for (let at = await fill.wantedChanged(); at !== undefined;
	 * at = await fill.wantedChanged())` serves each group as it becomes owed, and ends once the
	 * fill is answered or withdrawn.
	 */
	async wantedChanged(): Promise<bigint | undefined> {
		for (;;) {
			const wanted = this.wanted();
			if (!this.#prev || this.#prev.wanted !== wanted) {
				this.#prev = { wanted };
				return wanted;
			}
			await this.#pending.revision.changed();
		}
	}

	/** Resolves once no reader waits for group `sequence` from this fill, so stop serving it. */
	async unwanted(sequence: bigint): Promise<void> {
		checkSequence(sequence);
		while (this.isWanted(sequence)) await this.#pending.revision.changed();
	}

	/**
	 * Insert group `sequence` into the track's cache, handing it to the readers waiting on it,
	 * and return the producer to write its frames into. `info` commits the track's {@link Info}
	 * if it has none yet. Throws a `RangeError` outside the fill, and an error if the group was
	 * already answered or cached, or the track is closed.
	 */
	accept(sequence: bigint, info?: Info): GroupProducer {
		checkSequence(sequence);
		return fillOps.accept(this.#state, this.#pending, sequence, info);
	}

	/**
	 * Name `[start, end)` as never to be delivered: the source does not have them. Readers stop
	 * waiting for them, and the track records them so nobody asks again. Only the groups a reader
	 * still reads count; the rest of the fill was never owed.
	 */
	dropGroups(start: bigint, end: bigint): void {
		checkSequence(start);
		checkEnd(end);
		fillOps.drop(this.#state, this.#pending, start, end);
	}

	/**
	 * Refuse `[start, end)` with `error` without saying the source lost them, as when serving them
	 * failed for now. The readers waiting now stop waiting and get `error`; a later reader asks
	 * again.
	 */
	refuseGroups(start: bigint, end: bigint, error: Error): void {
		checkSequence(start);
		checkEnd(end);
		fillOps.refuse(this.#state, this.#pending, start, end, error);
	}

	/** Done serving: whatever a reader still wants is named dropped. Idempotent. */
	close(): void {
		fillOps.settle(this.#state, this.#pending);
	}

	[Symbol.dispose](): void {
		this.close();
	}
}
