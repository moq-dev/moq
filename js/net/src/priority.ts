/**
 * Group stream prioritization shared by the moq-lite and moq-transport publishers, mirroring
 * the Rust `lite::priority` queue.
 *
 * @module
 */

import type { Dispose } from "@moq/signals";
import type { Writer } from "./stream.ts";
import type * as track from "./track.ts";

// The transport compares send orders as a single number, so the ranks are packed into
// disjoint ranges: the subscriber's priority takes the high bits so it always dominates, the
// publisher's below it, and the position below that. Group data stays negative so protocol
// streams at the transport's default order of 0 always make progress. Two signed byte
// priorities and a 36-bit position fit the negative half of the safe integer range.
const GROUP_SPAN = 2 ** 36;

/**
 * What to rank a group stream by.
 *
 * @internal
 */
export interface Rank {
	/** The subscriber's track priority (`-128..=127`), which outweighs everything below it. */
	priority: number;

	/**
	 * The publisher's track priority (`-128..=127`), breaking ties between subscriptions at one
	 * subscriber priority. Defaults to 0, the unset priority.
	 */
	publisher?: number;

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
export function sendOrder({ priority, publisher = 0, position = 0 }: Rank): number {
	const rank = (127 - clamp(priority, -128, 127)) * 256 + (127 - clamp(publisher, -128, 127));
	return -(rank * GROUP_SPAN + clamp(position, 0, GROUP_SPAN - 1) + 1);
}

// Every rank is bounded: the priorities by their wire type, the position by the space left
// below them. Anything outside is a caller bug rather than a value worth preserving.
function clamp(value: number, min: number, max: number): number {
	return Math.min(Math.max(Math.trunc(value), min), max);
}

/**
 * Ranks one subscription's group streams against each other, and re-ranks them whenever the
 * set changes or the subscription is updated.
 *
 * A group's rank is its position among the groups in flight, so which group goes first is
 * decided here rather than by arithmetic on sequence numbers. The subscription's
 * {@link track.Order} picks the direction: newest first sheds a congested backlog rather than
 * the live edge, oldest first drains it.
 *
 * One listener covers the whole subscription. A listener per group would instead pile up on
 * it, and a track that stalls with many groups open would trip the signals leak guard.
 *
 * @internal
 */
export class Priority {
	#track: track.Subscriber;
	#publisher: number;
	#streams = new Map<Writer, { sequence: number; object: number }>();
	#dispose: Dispose;
	#closed = false;

	/**
	 * Follow `track`'s subscription until {@link close}, breaking ties at one subscriber
	 * priority by the track's `publisher` priority.
	 */
	constructor(track: track.Subscriber, publisher = 0) {
		this.#track = track;
		this.#publisher = publisher;
		this.#dispose = track.subscription.subscribe(() => this.#rerank());
	}

	/** The send order for a stream starting at this group and object, before it opens. */
	rank(sequence: number, object: number): number {
		const subscription = this.#track.subscription.peek();
		return sendOrder({
			priority: subscription?.priority ?? 0,
			publisher: this.#publisher,
			position: this.#position(sequence, object, subscription?.order),
		});
	}

	/** Rank a stream by its first object now and after ranking changes, until {@link remove}. */
	add(stream: Writer, sequence: number, object: number) {
		this.#streams.set(stream, { sequence, object });

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

	// Earlier objects win within one group, so a fill precedes its live rest. Group order
	// still dominates, and identical starts tie without counting themselves.
	#position(sequence: number, object: number, order: track.Order = "descending"): number {
		let ahead = 0;
		for (const other of this.#streams.values()) {
			if (
				other.sequence === sequence
					? other.object < object
					: order === "ascending"
						? other.sequence < sequence
						: other.sequence > sequence
			)
				ahead++;
		}
		return ahead;
	}

	#rerank() {
		const subscription = this.#track.subscription.peek();
		const priority = subscription?.priority ?? 0;
		for (const [stream, { sequence, object }] of this.#streams) {
			stream.setPriority(
				sendOrder({
					priority,
					publisher: this.#publisher,
					position: this.#position(sequence, object, subscription?.order),
				}),
			);
		}
	}
}
