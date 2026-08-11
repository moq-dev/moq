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
// group sequence breaks ties below it. The priority is a u8, so 45 bits are left for the
// sequence before the pack stops being an exact integer (255 * 2^45 + 2^45 - 1 is exactly
// Number.MAX_SAFE_INTEGER).
const GROUP_SPAN = 2 ** 45;

/** The highest track priority the wire carries (a u8). */
const MAX_PRIORITY = 0xff;

/** What to rank a group stream by. */
export interface Rank {
	/** The subscriber's track priority, which outweighs everything below it. */
	priority: number;

	/**
	 * The group's sequence, breaking ties within a track. Prefer a sequence relative to the
	 * subscription's first group over an absolute one: only the spread between the groups in
	 * flight has to fit the space left below the priority (see the note on `GROUP_SPAN`).
	 */
	sequence: number;

	/**
	 * Whether the subscriber wants groups in sequence order, oldest first (the `ordered`
	 * subscription option). Defaults to newest-first, which is what live playback wants.
	 */
	ordered?: boolean;
}

/**
 * The transport send order for a group, where HIGHER values are transmitted first.
 *
 * A higher `priority` always wins. Within a track, a higher `sequence` normally wins, so a
 * newer group preempts one that is falling behind. An `ordered` subscription inverts that:
 * the oldest group in flight goes first, since it is the one playback needs next.
 *
 * The sequence is a u53 on the wire and only 45 bits are left for it, so it enters modulo
 * that span. Two groups that far apart are never in flight together, so any monotonic
 * numbering keeps its order.
 */
export function sendOrder({ priority, sequence, ordered }: Rank): number {
	const group = wrap(sequence, GROUP_SPAN);
	return clamp(priority, MAX_PRIORITY) * GROUP_SPAN + (ordered ? GROUP_SPAN - 1 - group : group);
}

// The priority is bounded by its wire type, so anything outside it is a caller bug rather
// than a value to preserve.
function clamp(value: number, max: number): number {
	return Math.min(Math.max(Math.trunc(value), 0), max);
}

// The sequence has no such bound, and truncating it would rank every group above the cutoff
// equally. Wrapping instead keeps the ordering of any two groups that can coexist.
function wrap(value: number, span: number): number {
	return Math.max(Math.trunc(value), 0) % span;
}

/**
 * Ranks one subscription's group streams, re-ranking them whenever it is updated.
 *
 * A SUBSCRIBE_UPDATE changes the priority of every group in flight, so this holds a single
 * listener for the subscription and fans each change out to the streams. A listener per group
 * would instead pile up on the subscription, and a track that stalls with many groups open
 * would trip the signals leak guard.
 */
export class Priority {
	#track: track.Subscriber;
	#streams = new Map<Writer, number>();
	#dispose: Dispose;

	// The first sequence this subscription served, so ranks are a distance from it rather than
	// an absolute position. A publisher may number groups from a clock, and a relay preserves
	// whatever its upstream used, but the spread between the groups in flight is always small.
	#base?: number;

	/** Follow `track`'s subscription until {@link close}. */
	constructor(track: track.Subscriber) {
		this.#track = track;
		this.#dispose = track.subscription.subscribe(() => {
			for (const [stream, sequence] of this.#streams) {
				stream.setPriority(this.rank(sequence));
			}
		});
	}

	/** The send order for a group at `sequence`, given the subscription's current options. */
	rank(sequence: number): number {
		const subscription = this.#track.subscription.peek();
		this.#base ??= sequence;

		return sendOrder({
			priority: subscription?.priority ?? 0,
			// Groups are served in sequence order, so this only goes backwards if the publisher
			// renumbered, which leaves the group at the bottom of its priority.
			sequence: Math.max(0, sequence - this.#base),
			ordered: subscription?.ordered ?? false,
		});
	}

	/** Rank a group's stream now, and on every later update until {@link remove}. */
	add(stream: Writer, sequence: number) {
		this.#streams.set(stream, sequence);

		// Opening a stream can block on transport capacity, so an update that landed while it
		// did predates this registration.
		stream.setPriority(this.rank(sequence));
	}

	/** Stop ranking a finished group's stream. */
	remove(stream: Writer) {
		this.#streams.delete(stream);
	}

	/**
	 * Release the subscription listener.
	 *
	 * Any group still draining keeps the rank it last had, which is what a subscription on its
	 * way out wants anyway.
	 */
	close() {
		this.#dispose();
		this.#streams.clear();
	}
}
