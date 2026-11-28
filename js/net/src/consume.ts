import type * as broadcast from "./broadcast.ts";
import { StreamCode, StreamError } from "./error.ts";
import type { WireRoute } from "./hop.ts";
import * as Identity from "./identity.ts";
import { arrival, coveringPrefixes, mostSpecific } from "./internal.ts";
import type * as Path from "./path.ts";
import { type Served, sameIdentity } from "./wire.ts";

// Unroutable, as in Rust: the identity going away is an end, not a failure.
function retired(): StreamError {
	return new StreamError(StreamCode.Unroutable, { message: "broadcast identity retired" });
}

class Validity {
	#current = new AbortController();
	#answering = new AbortController();

	get signal(): AbortSignal {
		return this.#current.signal;
	}

	/**
	 * Aborted once another identity takes the path, which may answer a request still unanswered.
	 * A withdrawal alone leaves it, and the identity winning again keeps it.
	 */
	get answering(): AbortSignal {
		return this.#answering.signal;
	}

	update(allowed: boolean, replaced = false): void {
		if (allowed) {
			if (this.#current.signal.aborted) this.#current = new AbortController();
			if (this.#answering.signal.aborted) this.#answering = new AbortController();
			return;
		}
		if (!this.#current.signal.aborted) this.#current.abort(retired());
		if (replaced && !this.#answering.signal.aborted) this.#answering.abort(retired());
	}
}

// The route a consume selected: its identity, which its requests pin, and epoch.
type Selection = Served & {
	readonly validity?: Validity;
};

class Blind implements Selection {
	readonly ref = {};
	readonly route = {};
	readonly validity = new Validity();
}

type Advertised = { ref: object; route: WireRoute };

type CacheEntry = { consumer: broadcast.Consumer; selection: Selection };

/** Consumed broadcasts shared only within the selected serving identity. @internal */
export class BroadcastCache {
	#cache = new Map<Path.Valid, Map<Identity.Valid | object, CacheEntry>>();
	#announced = new Map<Path.Valid, Map<object, Advertised>>();
	#blind = new Map<Path.Valid, Blind>();
	#blindPrefixes = new Map<Path.Valid, Set<Path.Valid>>();
	#paths = new Map<Path.Valid, Set<Path.Valid>>();
	// The prefixes a batch changed, rechecked once it ends.
	#batched?: Set<Path.Valid>;

	/**
	 * Apply several changes as one: each path they touch is rechecked once, after the last, so
	 * a selection that only passes through another route on the way never retires a request.
	 */
	batch(apply: () => void): void {
		if (this.#batched) {
			apply();
			return;
		}
		const batched = new Set<Path.Valid>();
		this.#batched = batched;
		try {
			apply();
		} finally {
			this.#batched = undefined;
		}
		const paths = new Set<Path.Valid>();
		for (const prefix of batched) for (const path of this.#paths.get(prefix) ?? []) paths.add(path);
		for (const path of paths) this.#check(path);
	}

	/** Record this interest's live advertisement, preserving ref across route updates. */
	announce(path: Path.Valid, owner: object, route: WireRoute, restart = false): void {
		// Only unannounced consumers covered by this advertisement lose their blind ref.
		for (const covered of this.#blindPrefixes.get(path) ?? []) this.#forgetBlind(covered);
		let entries = this.#announced.get(path);
		if (!entries) {
			entries = new Map();
			this.#announced.set(path, entries);
		}
		const previous = entries.get(owner);
		const ref = (!restart && previous?.ref) || {};
		arrival(ref);
		// A new or restarted stream ranks as the newest, which wins a full tie at its own path; see mostSpecific.
		if (!previous || restart) entries.delete(owner);
		entries.set(owner, { ref, route });
		this.#update(path);
	}

	/** Withdraw this interest's advertisement without disturbing another interest's routes. */
	withdraw(path: Path.Valid, owner: object): void {
		const entries = this.#announced.get(path);
		entries?.delete(owner);
		if (entries?.size === 0) this.#announced.delete(path);
		this.#update(path);
	}

	/** Resolve the same most-specific-prefix and route preference as the origin. */
	identity(path: Path.Valid): Selection {
		const best = mostSpecific(
			path,
			(prefix) => this.#eligible(prefix).reverse(),
			(entry) => entry,
		);
		if (best) {
			const cached = this.#live(path, best);
			// A minted identity names the announcement, not the publisher's stream, so a
			// request for it can only name the path: it holds while this route is selected.
			const named = best.route.identity !== undefined && !Identity.isMinted(best.route.identity);
			return {
				...best,
				validity: !named ? (cached?.selection.validity ?? new Validity()) : undefined,
			};
		}
		return this.#blind.get(path) ?? new Blind();
	}

	/**
	 * The best route of `identity` alone at `path`, whichever identity wins the path, as a
	 * request pinned to it resolves. With no route of it, the identity alone, which the peer
	 * resolves or refuses.
	 */
	pinned(path: Path.Valid, identity: Identity.Valid): Selection {
		const best = mostSpecific(
			path,
			(prefix) =>
				this.#eligible(prefix)
					.filter((entry) => entry.route.identity === identity)
					.reverse(),
			(entry) => entry,
		);
		return best ?? { ref: {}, route: { identity } };
	}

	/** Clone the live cached consumer only when it serves the selected identity and epoch. */
	get(path: Path.Valid, identity: Served): broadcast.Consumer | undefined {
		return this.#live(path, identity)?.consumer.clone();
	}

	// The live cached entry consumed through `served`'s identity and epoch: an older epoch never
	// answers a fresh request, since the newer one may refuse it.
	#live(path: Path.Valid, served: Served): CacheEntry | undefined {
		const cached = this.#cache.get(path)?.get(served.route.identity ?? served.ref);
		if (cached?.consumer.closed.peek() !== undefined) return undefined;
		return cached?.selection.route.epoch === served.route.epoch ? cached : undefined;
	}

	/** Cache each live identity so a temporary route winner never displaces a held fallback. */
	insert(path: Path.Valid, identity: Selection, consumer: broadcast.Consumer): broadcast.Consumer {
		let entries = this.#cache.get(path);
		if (!entries) {
			entries = new Map();
			this.#cache.set(path, entries);
		}
		const key = identity.route.identity ?? identity.ref;
		const entry = { consumer, selection: identity };
		entries.set(key, entry);
		for (const prefix of coveringPrefixes(path)) {
			let paths = this.#paths.get(prefix);
			if (!paths) {
				paths = new Set();
				this.#paths.set(prefix, paths);
			}
			paths.add(path);
		}
		if (identity instanceof Blind) {
			this.#blind.set(path, identity);
			for (const prefix of coveringPrefixes(path)) {
				let paths = this.#blindPrefixes.get(prefix);
				if (!paths) {
					paths = new Set();
					this.#blindPrefixes.set(prefix, paths);
				}
				paths.add(path);
			}
		}
		void consumer.closed.then(() => {
			identity.validity?.update(false);
			if (entries.get(key) !== entry) return;
			entries.delete(key);
			if (entries.size === 0 && this.#cache.get(path) === entries) this.#forgetPath(path);
			if (this.#blind.get(path) === identity) this.#forgetBlind(path);
		});
		return consumer;
	}

	#forgetBlind(path: Path.Valid): void {
		const blind = this.#blind.get(path);
		if (!blind) return;
		blind.validity.update(false);
		this.#blind.delete(path);
		const entries = this.#cache.get(path);
		entries?.delete(blind.ref);
		if (entries?.size === 0) this.#forgetPath(path);
		for (const prefix of coveringPrefixes(path)) {
			const paths = this.#blindPrefixes.get(prefix);
			paths?.delete(path);
			if (paths?.size === 0) this.#blindPrefixes.delete(prefix);
		}
	}

	#forgetPath(path: Path.Valid): void {
		this.#cache.delete(path);
		for (const prefix of coveringPrefixes(path)) {
			const paths = this.#paths.get(prefix);
			paths?.delete(path);
			if (paths?.size === 0) this.#paths.delete(prefix);
		}
	}

	// The advertisements at `prefix`, oldest first.
	#eligible(prefix: Path.Valid): Advertised[] {
		return [...(this.#announced.get(prefix)?.values() ?? [])];
	}

	#update(prefix: Path.Valid): void {
		if (this.#batched) {
			this.#batched.add(prefix);
			return;
		}
		for (const path of this.#paths.get(prefix) ?? []) this.#check(path);
	}

	#check(path: Path.Valid): void {
		const selected = this.identity(path);
		for (const entry of this.#cache.get(path)?.values() ?? []) {
			const same = sameIdentity(entry.selection, selected);
			entry.selection.validity?.update(same, !same && !(selected instanceof Blind));
		}
	}
}
