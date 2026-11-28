/**
 * Group stream prioritization shared by the moq-lite and moq-transport publishers, mirroring
 * the Rust `lite::priority` queue.
 *
 * @module
 */

import type { Dispose, Getter } from "@moq/signals";
import { hooks } from "./internal.ts";
import type { Policy } from "./lite/subscribe.ts";
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
 * subscription rather than by its key, so two tracks at the same priority interleave:
 * each one's next group ties with the other's, whatever their group numbering. Comparing
 * keys instead would let a track that had been running longer, or that numbered its
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
 * A group stream's own ranking, for one whose policy can differ from its subscription's:
 * lite-07 ranks each group by the ranges still wanting it.
 *
 * @internal
 */
export interface Ranking {
	/** The policy the stream goes out at now, `whole` being the subscription's. */
	policy(whole: Policy): Policy;
	/** What besides the subscription can change {@link policy}, watched while the stream is ranked. */
	readonly changed: readonly Getter<unknown>[];
}

// Where a group stream starts, its group and its first object in that group, with the ranking
// it has of its own and the policy it was last ranked at.
interface Start {
	key: bigint;
	object: number;
	ranking?: Ranking;
	policy: Policy;
}

// A stream ranked by its start: an open one, or a place held for one still waiting to open.
type Ranked = Pick<Writer, "setPriority"> | Place;

/**
 * A group stream's place while it waits for stream credit: it ranks the subscription's other
 * streams as an open one would, so a newer group overtakes an older one still waiting.
 *
 * @internal
 */
export interface Place {
	/** The send order the stream would open at now. */
	rank(): number;
	/** Give the place up, once the stream opened or will not. */
	release(): void;
}

/**
 * Ranks one subscription's group streams against each other, and re-ranks them whenever the
 * set changes, the subscription is updated, or a stream's own {@link Ranking} may move.
 *
 * A group's rank is its policy's priority, then its position among the groups in flight at
 * that priority, so which group goes first is decided here rather than by arithmetic on
 * sequence numbers. The policy's {@link track.Order} picks the direction: newest first sheds a
 * congested backlog rather than the live edge, oldest first drains it.
 *
 * One listener covers the whole subscription, plus one per signal the streams' rankings watch
 * however many share it. A listener per group would instead pile up on the subscription, and a
 * track that stalls with many groups open would trip the signals leak guard.
 *
 * @internal
 */
export class Priority {
	#track: track.Subscriber;
	#publisher: number;
	#streams = new Map<Ranked, Start>();
	// Each signal the ranked streams' rankings watch, with how many do.
	#watched = new Map<Getter<unknown>, { count: number; dispose: Dispose }>();
	#dispose: Dispose;
	#closed = false;

	/**
	 * Follow `track`'s subscription until {@link close}, breaking ties at one subscriber
	 * priority by the track's `publisher` priority.
	 */
	constructor(track: track.Subscriber, publisher = 0) {
		this.#track = track;
		this.#publisher = publisher;
		this.#dispose = hooks.preferences(track).revision.subscribe(() => this.#rerank());
	}

	/**
	 * The send order for a stream starting at this group and object, before it opens, at
	 * `ranking`'s policy when it has one of its own.
	 */
	rank(key: bigint, object: number, ranking?: Ranking): number {
		const whole = this.#whole();
		const start = { key, object, policy: ranking?.policy(whole) ?? whole };
		let position = 0;
		for (const other of this.#streams.values()) {
			if (other.policy.priority === start.policy.priority && compareStarts(other, start) < 0) position++;
		}
		return sendOrder({ priority: start.policy.priority, publisher: this.#publisher, position });
	}

	/** Hold a place for a stream starting at this group and object while it waits to open. */
	reserve(key: bigint, object: number, ranking?: Ranking): Place {
		const place: Place = {
			rank: () => this.rank(key, object, ranking),
			release: () => this.remove(place),
		};
		this.#insert(place, key, object, ranking);
		return place;
	}

	/**
	 * Rank a stream by its first object, at `ranking`'s policy when it has one of its own, now
	 * and after ranking changes, until {@link remove}.
	 */
	add(stream: Writer, key: bigint, object: number, ranking?: Ranking) {
		// Also covers the stream itself: opening one can block on transport capacity, so an
		// update that landed while it did predates this registration.
		this.#insert(stream, key, object, ranking);
	}

	/** Stop ranking a finished group's stream, promoting whatever was queued behind it. */
	remove(stream: Ranked) {
		const start = this.#streams.get(stream);
		if (!start) return;
		this.#streams.delete(stream);
		for (const getter of start.ranking?.changed ?? []) {
			const watched = this.#watched.get(getter);
			if (!watched) continue;
			watched.count--;
			if (watched.count > 0) continue;
			watched.dispose();
			this.#watched.delete(getter);
		}

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

	#insert(stream: Ranked, key: bigint, object: number, ranking?: Ranking) {
		this.#streams.set(stream, { key, object, ranking, policy: this.#whole() });
		for (const getter of ranking?.changed ?? []) {
			const watched = this.#watched.get(getter);
			if (watched) watched.count++;
			else this.#watched.set(getter, { count: 1, dispose: getter.subscribe(() => this.#rerank()) });
		}
		this.#rerank();
	}

	// The subscription's own policy, which a stream without a ranking of its own goes out at.
	#whole(): Policy {
		return hooks.preferences(this.#track).policy();
	}

	// Sorted once, so ranking every stream costs one sort rather than a scan per stream: a
	// stream's position is how many at its priority sort strictly ahead of it.
	#rerank() {
		const whole = this.#whole();
		for (const start of this.#streams.values()) start.policy = start.ranking?.policy(whole) ?? whole;
		const queue = [...this.#streams].sort(([, a], [, b]) => compareStarts(a, b));
		let first = 0;
		let position = 0;
		for (let i = 0; i < queue.length; i++) {
			const [stream, start] = queue[i];
			const prev = queue[i - 1]?.[1];
			if (prev?.policy.priority !== start.policy.priority) {
				first = i;
				position = 0;
			} else if (compareStarts(prev, start) !== 0) {
				position = i - first;
			}
			if ("setPriority" in stream)
				stream.setPriority(
					sendOrder({ priority: start.policy.priority, publisher: this.#publisher, position }),
				);
		}
	}
}

// Negative when `a` goes before `b`. A higher priority goes first. Within one, each start's
// own order picks the direction, newest first when descending and oldest first when
// ascending, so starts of two orders compare by key against negated key. Earlier objects win
// within one group so a fill precedes its live rest, and identical starts tie.
function compareStarts(a: Start, b: Start): number {
	if (a.policy.priority !== b.policy.priority) return b.policy.priority - a.policy.priority;
	const signed = (start: Start) => (start.policy.order === "ascending" ? -start.key : start.key);
	const [x, y] = [signed(a), signed(b)];
	if (x !== y) return x > y ? -1 : 1;
	return a.object - b.object;
}
