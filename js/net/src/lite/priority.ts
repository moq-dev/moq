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

/**
 * The transport send order for a group, where HIGHER values are transmitted first.
 *
 * A higher `priority` (the subscriber's track priority) always wins; a higher `sequence`
 * only breaks ties within the same track, so a newer group preempts an older one.
 *
 * The sequence is a u53 on the wire, so it enters modulo the space left for it. A send
 * order only ranks the streams that are in flight together, and no two of those are 2^45
 * groups apart, so any monotonic numbering (including one seeded from a clock or preserved
 * across a relay) keeps its order.
 */
export function sendOrder(priority: number, sequence: number): number {
	return clamp(priority, MAX_PRIORITY) * GROUP_SPAN + wrap(sequence, GROUP_SPAN);
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

	/** Follow `track`'s subscription until {@link close}. */
	constructor(track: track.Subscriber) {
		this.#track = track;
		this.#dispose = track.subscription.subscribe(() => {
			for (const [stream, sequence] of this.#streams) {
				stream.setPriority(this.rank(sequence));
			}
		});
	}

	/** The send order for a group at `sequence`, given the subscription's current priority. */
	rank(sequence: number): number {
		return sendOrder(this.#track.subscription.peek()?.priority ?? 0, sequence);
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
