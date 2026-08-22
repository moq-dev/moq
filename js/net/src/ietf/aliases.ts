const TRACK_ALIAS_TIMEOUT_MS = 1000;

/**
 * How many cancelled aliases to remember. Objects keep arriving for about a round trip
 * after we cancel, so a handful covers the window, while the cap keeps a long session with
 * heavy subscription churn from accumulating tombstones for its whole lifetime.
 *
 * The bound is a count rather than a deadline, so eviction stays synchronous with
 * retirement instead of needing a timer. The trade is that a session cancelling more than
 * this many distinct aliases inside one round trip evicts a tombstone whose objects are
 * still arriving; those fall back to the unknown-alias wait, which is the old behavior
 * rather than a new failure.
 */
const RETIRED_ALIAS_CAPACITY = 64;

type Resolver<T> = PromiseWithResolvers<T>["resolve"];

/**
 * The full track name an alias is bound to.
 *
 * Kept as two components rather than one joined string: a track name may contain the same
 * separator a broadcast path uses, so `a` + `b/c` and `a/b` + `c` would otherwise compare
 * equal and turn a genuine collision into apparent sharing.
 * @internal
 */
export type TrackIdentity = { broadcast: string; name: string };

function sameTrack(a: TrackIdentity, b: TrackIdentity): boolean {
	return a.broadcast === b.broadcast && a.name === b.name;
}

/**
 * Thrown when a group arrives for a subscription we already cancelled.
 *
 * The publisher only stops once our cancellation reaches it, so objects keep arriving for
 * at least a round trip afterwards. Draft-19 section 11.1 asks us to keep enough state to
 * discard them quickly "rather than treating them as belonging to an unknown Track Alias".
 *
 * This makes the window quiet, not safe. Once a publisher reassigns the alias the tombstone
 * is reclaimed, and nothing on a group stream distinguishes the old subscription's objects
 * from the new one's. Cancelling promptly is what bounds the exposure to a round trip.
 * @internal
 */
export class RetiredTrackAlias extends Error {
	constructor(alias: bigint) {
		super(`track alias retired: ${alias}`);
		this.name = "RetiredTrackAlias";
	}
}

/**
 * Thrown when a publisher gives one alias to two subscriptions of the same track.
 *
 * Draft-19 section 5.1 permits this and expects the subscriber to demux by re-applying
 * each subscription's filter. Ours are all LargestObject, so they are indistinguishable and
 * we cannot, which costs the one subscription. It is not a protocol error, so it must not
 * take the session down with it.
 * @internal
 */
export class SharedTrackAlias extends Error {
	constructor(alias: bigint) {
		super(`track alias shared by another subscription: ${alias}`);
		this.name = "SharedTrackAlias";
	}
}

/**
 * Thrown when a publisher points one alias at two different tracks at once, which
 * draft-19 section 11.1 makes fatal to the session.
 * @internal
 */
export class DuplicateTrackAlias extends Error {
	constructor(alias: bigint) {
		super(`duplicate track alias: ${alias}`);
		this.name = "DuplicateTrackAlias";
	}
}

/** Resolves publisher-chosen track aliases after control/data stream reordering. @internal */
export class TrackAliases<T> {
	#active = new Map<bigint, { value: T; track: TrackIdentity }>();
	#pending = new Map<bigint, Set<Resolver<T>>>();

	/** Aliases whose subscription we cancelled, in retirement order so the oldest is forgotten first. */
	#retired: bigint[] = [];
	#retiredSet = new Set<bigint>();

	/**
	 * Waits briefly for an alias to be established by SUBSCRIBE_OK or PUBLISH.
	 *
	 * Throws {@link RetiredTrackAlias} at once for an alias we cancelled, rather than
	 * waiting out the timeout for a binding that is never coming.
	 */
	async get(alias: bigint): Promise<T> {
		const bound = this.#active.get(alias);
		if (bound !== undefined) return bound.value;
		if (this.#retiredSet.has(alias)) throw new RetiredTrackAlias(alias);

		const { promise, resolve } = Promise.withResolvers<T>();
		let resolvers = this.#pending.get(alias);
		if (!resolvers) {
			resolvers = new Set();
			this.#pending.set(alias, resolvers);
		}
		resolvers.add(resolve);

		let timer: ReturnType<typeof setTimeout> | undefined;
		const timeout = new Promise<never>((_, reject) => {
			timer = setTimeout(() => reject(new Error(`unknown track alias: ${alias}`)), TRACK_ALIAS_TIMEOUT_MS);
		});

		try {
			return await Promise.race([promise, timeout]);
		} finally {
			clearTimeout(timer);
			resolvers.delete(resolve);
			if (this.#pending.get(alias) === resolvers && resolvers.size === 0) this.#pending.delete(alias);
		}
	}

	/**
	 * Establishes an alias and releases any data streams waiting for it.
	 *
	 * `track` is the full track name the alias was bound to, which is what decides whether a
	 * repeat is the legal sharing of an alias across subscriptions to one track
	 * ({@link SharedTrackAlias}) or the collision that must fail the session
	 * ({@link DuplicateTrackAlias}).
	 */
	set(alias: bigint, value: T, track: TrackIdentity) {
		const active = this.#active.get(alias);
		if (active !== undefined) {
			if (active.value === value) return;
			throw sameTrack(active.track, track) ? new SharedTrackAlias(alias) : new DuplicateTrackAlias(alias);
		}

		// Our subscription is gone, so the publisher is free to point the alias somewhere
		// new. Reclaiming it also drops the tombstone early.
		this.#forget(alias);

		this.#active.set(alias, { value, track });
		const resolvers = this.#pending.get(alias);
		this.#pending.delete(alias);
		for (const resolve of resolvers ?? []) resolve(value);
	}

	/**
	 * Retires an alias whose subscription was cancelled, so groups still in flight for it
	 * are discarded promptly instead of reported as unknown.
	 *
	 * Only retires an alias that still belongs to the supplied value: a later subscription
	 * may already have reclaimed it, and that binding outranks a departing owner.
	 *
	 * Returns whether this caller still owned the binding, so metadata keyed by the alias is
	 * only torn down by the owner and never out from under whoever reclaimed it.
	 */
	retire(alias: bigint, value: T): boolean {
		if (this.#active.get(alias)?.value !== value) return false;
		this.#active.delete(alias);

		if (this.#retiredSet.has(alias)) return true;
		this.#retiredSet.add(alias);
		this.#retired.push(alias);

		while (this.#retired.length > RETIRED_ALIAS_CAPACITY) {
			const oldest = this.#retired.shift();
			if (oldest !== undefined) this.#retiredSet.delete(oldest);
		}

		return true;
	}

	#forget(alias: bigint) {
		if (!this.#retiredSet.delete(alias)) return;
		const at = this.#retired.indexOf(alias);
		if (at !== -1) this.#retired.splice(at, 1);
	}
}
