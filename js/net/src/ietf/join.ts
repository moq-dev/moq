import type * as netGroup from "../group.ts";
import type { Tail } from "../tail.ts";

/** The head and the Object ID its live tail must start at. */
export interface Head {
	producer: netGroup.Producer;
	next: number;
}

type State =
	/** The joining FETCH or the fill is out, or about to be: a head may still come. */
	| { kind: "pending" }
	/** The fetch stream is writing the head into the group. */
	| { kind: "writing"; producer: netGroup.Producer; next: number }
	/** The head is written and waits for the live tail to take the group over. */
	| { kind: "ready"; producer: netGroup.Producer; next: number }
	/** No head is coming: refused, empty, failed, claimed, or the subscription went. */
	| { kind: "done" }
	/** The track ended, so no tail is coming and a head is the whole group. */
	| { kind: "ended" };

/**
 * The join of one live group: the head a draft-18 joining FETCH or a draft-22 fill brings and
 * the live tail the subscription delivers, stitched into one group.
 *
 * A Next Object subscription starts after the Largest SUBSCRIBE_OK names, `{G, k}`, and the
 * head's fetch stream ends at it, so the head carries objects `0..=k` and the tail starts at
 * `k + 1`. The track has one producer per group, so the fetch stream creates group G and
 * writes the head as it arrives (readers see it at once: a catalog written once needs no
 * tail at all), and the tail takes that producer over here. A tail that arrives first waits.
 *
 * Mirrors the Rust subscriber's `Fill`. A refused, empty, or failed head costs only G's
 * head: its mid-group tail is dropped like any headless stream, and later groups are whole.
 *
 * @internal
 */
export class Join {
	/** The group this join brings the head of: SUBSCRIBE_OK's Largest group. */
	readonly sequence: number;

	#state: State = { kind: "pending" };
	// Tails parked on the head, woken synchronously so they hold the end before the head lets go.
	#waiters: (() => void)[] = [];
	// The subscription's end stays open while a head may still come, unless the Stream Count
	// already waits for the head's stream.
	#release: () => void;
	#tail: Tail;
	#settled: Promise<void>;
	#settle!: () => void;
	// A head still being written when the track ended, finished with its stream.
	#ended?: netGroup.Producer;

	/**
	 * `counted` says the head's stream is one of the subscription's own, which PUBLISH_DONE's
	 * Stream Count includes: a draft-22 fill. A draft-18 joining FETCH is a request of its own,
	 * so the join holds the end open for it instead.
	 */
	constructor(sequence: number, tail: Tail, { counted = false }: { counted?: boolean } = {}) {
		this.sequence = sequence;
		this.#tail = tail;
		this.#release = counted ? () => {} : tail.hold();
		this.#settled = new Promise((resolve) => {
			this.#settle = resolve;
		});
	}

	/** Whether a head may still arrive or waits to be claimed. */
	get outstanding(): boolean {
		const { kind } = this.#state;
		return kind === "pending" || kind === "writing" || kind === "ready";
	}

	/** Whether no head has started yet: nothing was refused, and no object arrived. */
	get pending(): boolean {
		return this.#state.kind === "pending";
	}

	/** Resolves once the head is written or none is coming. */
	get settled(): Promise<void> {
		return this.#settled;
	}

	/**
	 * The fetch stream's first object arrived: create the group for the head with `create`.
	 *
	 * Returns undefined when nothing should be written: a whole group already took the
	 * sequence, the join gave up, or a second stream answers the one request.
	 */
	open(create: () => netGroup.Producer): netGroup.Producer | undefined {
		const { kind } = this.#state;
		// After the track ended the head still lands, as the whole group.
		if (kind !== "pending" && kind !== "ended") return undefined;
		const producer = create();
		if (kind === "ended") {
			this.#ended = producer;
			return producer;
		}
		this.#state = { kind: "writing", producer, next: 0 };
		return producer;
	}

	/** The head wrote its next object. */
	wrote(): void {
		if (this.#state.kind === "writing") this.#state.next += 1;
	}

	/** The fetch stream ended cleanly: the head waits for its tail, or none came at all. */
	finish(): void {
		if (this.#ended) {
			this.#ended.finish();
			this.#ended = undefined;
			return;
		}
		const state = this.#state;
		if (state.kind === "writing") {
			this.#transition({ kind: "ready", producer: state.producer, next: state.next });
		} else if (state.kind === "pending") {
			// An empty fetch stream: there is no head.
			this.#transition({ kind: "done" });
		}
	}

	/** The fetch stream failed: a half-written head is a group with no end. */
	fail(err: Error): void {
		if (this.#ended) {
			this.#ended.abort(err);
			this.#ended = undefined;
			return;
		}
		const state = this.#state;
		if (state.kind === "writing" || state.kind === "ready") state.producer.abort(err);
		if (state.kind !== "ended") this.#transition({ kind: "done" });
	}

	/**
	 * The joining FETCH was refused or its request failed. A head already written stands;
	 * one still being written cannot be trusted to finish.
	 */
	refuse(err: Error): void {
		const state = this.#state;
		if (state.kind === "writing") {
			state.producer.abort(err);
			this.#transition({ kind: "done" });
		} else if (state.kind === "pending") {
			this.#transition({ kind: "done" });
		}
	}

	/**
	 * A live stream for this group starts at object 0, so it carries the whole group.
	 *
	 * Before the head it wins the sequence, and the head is dropped when it lands. After it,
	 * the publisher served those objects twice and the track has one producer per group, so
	 * both are refused rather than cache a cut group as complete. Returns whether the whole
	 * group may stand. Matches the Rust subscriber.
	 */
	whole(): boolean {
		const state = this.#state;
		if (state.kind === "pending") {
			this.#transition({ kind: "done" });
			return true;
		}
		if (state.kind === "writing" || state.kind === "ready") {
			console.warn(`a whole group arrived for one its fetch stream already headed: group=${this.sequence}`);
			state.producer.abort(new Error("the publisher served the group's head twice"));
			this.#transition({ kind: "done" });
			return false;
		}
		return true;
	}

	/**
	 * Take the head over for a live tail starting at Object ID `start`, or `undefined` for a
	 * tail with no objects, which ends the group wherever the head did.
	 *
	 * Waits while the head is being fetched, meanwhile not holding the subscription's end open
	 * through `suspend`: the head holds it instead. Returns undefined when there is no head to
	 * continue, and drops a head that ends somewhere other than where the tail starts, since
	 * that hole is something the model cannot express.
	 */
	async claim(start: number | undefined, suspend?: () => () => void): Promise<Head | undefined> {
		const { kind } = this.#state;
		if (kind === "pending" || kind === "writing") {
			const resume = suspend?.();
			await new Promise<void>((resolve) => {
				this.#waiters.push(() => {
					resume?.();
					resolve();
				});
			});
		}

		const state = this.#state;
		if (state.kind !== "ready") return undefined;
		if (start !== undefined && start !== state.next) {
			console.warn(
				`the head does not meet the live tail: group=${this.sequence} head=${state.next} tail=${start}`,
			);
			state.producer.abort(new Error("the head and the live tail do not meet"));
			this.#transition({ kind: "done" });
			return undefined;
		}
		this.#transition({ kind: "done" });
		return { producer: state.producer, next: state.next };
	}

	/**
	 * The track ended and the subscription's streams are accounted for, so no tail is coming:
	 * a head nothing claimed is the whole group, and one still being written will be.
	 */
	end(): void {
		const state = this.#state;
		if (state.kind === "ready") state.producer.finish();
		if (state.kind === "writing") this.#ended = state.producer;
		if (state.kind !== "done") this.#transition({ kind: "ended" });
	}

	/**
	 * The subscription went without the track ending. Abort a head nothing claimed rather than
	 * finish it: the group may still be open upstream, and a later join can serve it whole.
	 */
	cancel(err: Error): void {
		const state = this.#state;
		if (state.kind === "writing" || state.kind === "ready") state.producer.abort(err);
		this.#ended?.abort(err);
		this.#ended = undefined;
		if (state.kind !== "done" && state.kind !== "ended") this.#transition({ kind: "done" });
	}

	#transition(next: State): void {
		this.#state = next;
		if (next.kind === "writing") return;
		// Out of pending or writing: the head is settled, so wake its tails before letting go
		// of the end, which they then hold themselves.
		const waiters = this.#waiters;
		this.#waiters = [];
		for (const wake of waiters) wake();
		this.#settle();
		this.#release();
		this.#tail.touch();
	}
}
