/**
 * Stream prioritization for moq-lite, mirroring the Rust `lite::priority` queue.
 *
 * @module
 */

import type { Dispose } from "@moq/signals";
import type { Writer } from "../stream.ts";
import type * as track from "../track.ts";

// The transport compares send orders as a single number, so the two ranks are packed into
// disjoint ranges: the track priority takes the high bits so it always dominates, and the
// position below it. Group data stays negative so protocol streams at the transport's default
// order of 0 always make progress. A u8 priority and 44-bit position fill the negative half of
// the safe integer range exactly.
const GROUP_SPAN = 2 ** 44;

/** The highest track priority the wire carries (a u8). */
const MAX_PRIORITY = 0xff;

/**
 * What to rank a group stream by.
 *
 * @internal
 */
export interface Rank {
	/** The subscriber's track priority, which outweighs everything below it. */
	priority: number;

	/**
	 * Where the group sits in the queue its own subscription wants sent, 0 being the one to
	 * send next. Defaults to 0, which is right for a stream that is the only one of its kind.
	 */
	position?: number;
}

/**
 * The transport send order for a group, where HIGHER values are transmitted first.
 *
 * A higher `priority` always wins. Below it, a group is ranked by its `position` in its own
 * subscription rather than by its sequence, so two tracks at the same priority interleave:
 * each one's next group ties with the other's, whatever their group numbering. Comparing
 * sequences instead would let a track that had been running longer, or that numbered its
 * groups from a clock, starve one that started later.
 *
 * @internal
 */
export function sendOrder({ priority, position = 0 }: Rank): number {
	return -((MAX_PRIORITY - clamp(priority, MAX_PRIORITY)) * GROUP_SPAN + clamp(position, GROUP_SPAN - 1) + 1);
}

// Both ranks are bounded: the priority by its wire type, the position by the space left below
// it. Anything outside is a caller bug rather than a value worth preserving.
function clamp(value: number, max: number): number {
	return Math.min(Math.max(Math.trunc(value), 0), max);
}

/**
 * Ranks one subscription's group streams against each other, and re-ranks them whenever the
 * set changes or the subscription is updated.
 *
 * A group's rank is its position among the groups in flight, so which group goes first is
 * decided here rather than by arithmetic on sequence numbers. Newest-first by default, since
 * that is what live playback wants; an `ordered` subscription is playing through in sequence,
 * so it gets the oldest group first instead.
 *
 * One listener covers the whole subscription. A listener per group would instead pile up on
 * it, and a track that stalls with many groups open would trip the signals leak guard.
 *
 * @internal
 */
export class Priority {
	#track: track.Subscriber;
	#streams = new Map<Writer, number>();
	#dispose: Dispose;
	#closed = false;

	/** Follow `track`'s subscription until {@link close}. */
	constructor(track: track.Subscriber) {
		this.#track = track;
		this.#dispose = track.subscription.subscribe(() => this.#rerank());
	}

	/** The send order for a group at `sequence`, whether or not it has a stream yet. */
	rank(sequence: number): number {
		return sendOrder({
			priority: this.#track.subscription.peek()?.priority ?? 0,
			position: this.#position(sequence),
		});
	}

	/** Rank a group's stream now, and again whenever the ranking changes, until {@link remove}. */
	add(stream: Writer, sequence: number) {
		this.#streams.set(stream, sequence);

		// Also covers the stream itself: opening one can block on transport capacity, so an
		// update that landed while it did predates this registration.
		this.#rerank();
	}

	/** Stop ranking a finished group's stream, promoting whatever was queued behind it. */
	remove(stream: Writer) {
		if (!this.#streams.delete(stream)) return;

		if (this.#closed && this.#streams.size === 0) {
			this.#dispose();
		} else {
			this.#rerank();
		}
	}

	/**
	 * Stop taking new groups, and release the subscription listener once the last one leaves.
	 *
	 * The track can finish while several groups are still draining, and those are the ones the
	 * subscriber is still waiting on. They keep being promoted as the groups ahead of them
	 * finish, rather than being stranded at whatever position they held when the track ended.
	 */
	close() {
		this.#closed = true;
		if (this.#streams.size === 0) this.#dispose();
	}

	// How many of this subscription's groups in flight should be sent before `sequence`.
	// Sequences are unique within a subscription, so a group already registered never counts
	// itself.
	#position(sequence: number): number {
		const ordered = this.#track.subscription.peek()?.ordered ?? false;

		let ahead = 0;
		for (const other of this.#streams.values()) {
			if (ordered ? other < sequence : other > sequence) ahead++;
		}
		return ahead;
	}

	#rerank() {
		const priority = this.#track.subscription.peek()?.priority ?? 0;
		for (const [stream, sequence] of this.#streams) {
			stream.setPriority(sendOrder({ priority, position: this.#position(sequence) }));
		}
	}
}
