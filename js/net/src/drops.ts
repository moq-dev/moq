/**
 * A track's drop set: the sequences its publisher gave up, with why, as runs. Mirrors the Rust
 * `TrackState::drops` with its change index.
 *
 * Package-internal: not exported from the entrypoint.
 *
 * @module
 */
import { Signal } from "@moq/signals";
import { type DropRun, type Run, sameHole } from "./internal.ts";

/**
 * Sorted, disjoint runs, each replacing whatever it overlaps, with neighbours that say the same
 * thing merged. Every change costs the runs it touches and a binary search, never the set.
 */
export class Runs<R extends Run> {
	#runs: R[] = [];
	#same: (a: R, b: R) => boolean;

	constructor(same: (a: R, b: R) => boolean) {
		this.#same = same;
	}

	/** Every run, lowest first. */
	get all(): readonly R[] {
		return this.#runs;
	}

	/** How many runs are held. */
	get size(): number {
		return this.#runs.length;
	}

	/** The run holding `sequence`, if any. */
	at(sequence: bigint): R | undefined {
		const run = this.#runs[this.#above(sequence)];
		return run && run.start <= sequence ? run : undefined;
	}

	/** The runs overlapping `[start, end)`, lowest first. */
	overlapping(start: bigint, end: bigint): R[] {
		const found: R[] = [];
		for (let i = this.#above(start); i < this.#runs.length && this.#runs[i].start < end; i++)
			found.push(this.#runs[i]);
		return found;
	}

	/**
	 * Set `[start, end)` to `run`, or clear it when `run` is undefined. Returns the runs taken out
	 * and the ones put in their place, the neighbours a merge or split touched included.
	 */
	set(start: bigint, end: bigint, run?: R): { removed: R[]; added: R[] } {
		if (start >= end) return { removed: [], added: [] };
		const runs = this.#runs;
		// Touching neighbours are taken out too, so an alike one merges.
		let first = this.#above(start);
		if (first > 0 && runs[first - 1].end === start) first--;
		let last = first;
		while (last < runs.length && runs[last].start <= end) last++;
		const removed = runs.slice(first, last);
		const pieces: R[] = [];
		const lowest = removed[0];
		if (lowest && lowest.start < start) pieces.push({ ...lowest, end: start < lowest.end ? start : lowest.end });
		if (run) pieces.push({ ...run, start, end });
		const highest = removed[removed.length - 1];
		if (highest && highest.end > end) pieces.push({ ...highest, start: end > highest.start ? end : highest.start });
		const added: R[] = [];
		for (const piece of pieces) {
			const before = added[added.length - 1];
			if (before && before.end === piece.start && this.#same(before, piece)) before.end = piece.end;
			else added.push(piece);
		}
		runs.splice(first, last - first, ...added);
		return { removed, added };
	}

	// The index of the first run ending past `sequence`.
	#above(sequence: bigint): number {
		let lo = 0;
		let hi = this.#runs.length;
		while (lo < hi) {
			const mid = (lo + hi) >>> 1;
			if (this.#runs[mid].end <= sequence) lo = mid + 1;
			else hi = mid;
		}
		return lo;
	}
}

// Dead entries the change log tolerates beyond twice the live runs before it is rebuilt.
const LOG_SLACK = 64;

/**
 * The sequences a track's publisher gave up, with why, shared by the track and every reader of
 * it, plus an index of each run's last change, so a reader folds in only what changed since it
 * last looked.
 */
export class DropSet {
	readonly #runs = new Runs<DropRun>(sameHole);
	// Each run's last change, oldest first. An entry whose run was since replaced is dead, and
	// the log is rebuilt once dead entries clearly outnumber live ones.
	#log: { revision: number; run: DropRun }[] = [];
	#live = new Set<DropRun>();
	#revision = 0;

	/** Bumped on every change, for a reader to wake on. */
	readonly version = new Signal(0);

	/** Every run, lowest first. */
	get all(): readonly DropRun[] {
		return this.#runs.all;
	}

	/** How many runs are held. */
	get size(): number {
		return this.#runs.size;
	}

	/** The revision of the latest change, for {@link since}. */
	get revision(): number {
		return this.#revision;
	}

	/** The run holding `sequence`, if any. */
	at(sequence: bigint): DropRun | undefined {
		return this.#runs.at(sequence);
	}

	/** The runs overlapping `[start, end)`, lowest first. */
	overlapping(start: bigint, end: bigint): DropRun[] {
		return this.#runs.overlapping(start, end);
	}

	/** Give up `[start, end)` as `run` says, or forget it when `run` is undefined. */
	set(start: bigint, end: bigint, run?: Omit<DropRun, "start" | "end">): void {
		const { removed, added } = this.#runs.set(start, end, run && { ...run, start, end });
		if (removed.length === 0 && added.length === 0) return;
		for (const old of removed) this.#live.delete(old);
		for (const fresh of added) {
			this.#live.add(fresh);
			this.#log.push({ revision: ++this.#revision, run: fresh });
		}
		if (added.length === 0) this.#revision++;
		if (this.#log.length > 2 * this.#live.size + LOG_SLACK) {
			this.#log = this.#log.filter((entry) => this.#live.has(entry.run));
		}
		this.version.update((version) => version + 1);
	}

	/**
	 * The runs changed since `revision`, as their current extent. A run forgotten outright leaves
	 * no entry, so a reader checks what it folded in against {@link at} before reporting it.
	 */
	since(revision: number): Run[] {
		let lo = 0;
		let hi = this.#log.length;
		while (lo < hi) {
			const mid = (lo + hi) >>> 1;
			if (this.#log[mid].revision <= revision) lo = mid + 1;
			else hi = mid;
		}
		const changed: Run[] = [];
		for (let i = lo; i < this.#log.length; i++) {
			const { run } = this.#log[i];
			if (this.#live.has(run)) changed.push({ start: run.start, end: run.end });
		}
		return changed;
	}
}
