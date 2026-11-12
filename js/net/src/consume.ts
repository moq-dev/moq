import type * as broadcast from "./broadcast.ts";
import { StreamCode, StreamError } from "./error.ts";
import type { WireRoute } from "./hop.ts";
import * as Instance from "./instance.ts";
import { bestCandidate, coveringPrefixes } from "./internal.ts";
import type * as Path from "./path.ts";
import { type Served, sameInstance } from "./wire.ts";

// Unroutable, as in Rust: the instance going away is an end, not a failure.
function retired(): StreamError {
	return new StreamError(StreamCode.Unroutable, { message: "broadcast instance retired" });
}

class Validity {
	#current = new AbortController();
	#answering = new AbortController();

	get signal(): AbortSignal {
		return this.#current.signal;
	}

	/**
	 * Aborted once another instance takes the path, which may answer a request still unanswered.
	 * A withdrawal alone leaves it, and the instance winning again keeps it.
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

// The route a consume selected: its instance, and the origin node its requests name (lite-07).
type Selection = Served & { readonly route: Pick<WireRoute, "instance" | "node">; readonly validity?: Validity };

class Blind implements Selection {
	readonly identity = {};
	readonly route = {};
	readonly validity = new Validity();
}

/** Which claims above a consumed path may serve it: lite-07's floors. @internal */
export interface Covering {
	/** Whether the claim of origin `node` at `at` may serve a consume of `path` below it. */
	covers(node: bigint, at: Path.Valid, path: Path.Valid): boolean;
}

type Advertised = { identity: object; route: WireRoute; parked?: boolean };

/** Consumed broadcasts shared only within the selected serving instance. @internal */
export class BroadcastCache {
	#cache = new Map<
		Path.Valid,
		Map<Instance.Valid | object, { consumer: broadcast.Consumer; selection: Selection }>
	>();
	#announced = new Map<Path.Valid, Map<object, Advertised>>();
	#blind = new Map<Path.Valid, Blind>();
	#blindPrefixes = new Map<Path.Valid, Set<Path.Valid>>();
	#paths = new Map<Path.Valid, Set<Path.Valid>>();
	readonly #covering?: Covering;

	constructor(covering?: Covering) {
		this.#covering = covering;
	}

	/** Record this interest's live advertisement, preserving identity across route updates. */
	announce(path: Path.Valid, owner: object, route: WireRoute, restart = false): void {
		// Only unannounced consumers covered by this advertisement lose their blind identity.
		for (const covered of this.#blindPrefixes.get(path) ?? []) this.#forgetBlind(covered);
		let entries = this.#announced.get(path);
		if (!entries) {
			entries = new Map();
			this.#announced.set(path, entries);
		}
		const previous = entries.get(owner);
		const identity = (!restart && previous?.identity) || {};
		// A new or restarted stream ranks as the newest, which wins a full tie; see bestCandidate.
		if (!previous || restart) entries.delete(owner);
		entries.set(owner, { identity, route });
		this.#update(path);
	}

	/**
	 * Keep this interest's advertisement in its place among a tie while no consume may select it,
	 * until it is announced again without a restart: a lite-07 route that cannot be used for now.
	 */
	park(path: Path.Valid, owner: object): void {
		const entry = this.#announced.get(path)?.get(owner);
		if (!entry || entry.parked) return;
		entry.parked = true;
		this.#update(path);
	}

	/** Re-check every consume's selection, once what covers a path changed without an announcement. */
	revalidate(): void {
		for (const path of this.#cache.keys()) this.#check(path);
	}

	/** Withdraw this interest's advertisement without disturbing another interest's routes. */
	withdraw(path: Path.Valid, owner: object): void {
		const entries = this.#announced.get(path);
		entries?.delete(owner);
		if (entries?.size === 0) this.#announced.delete(path);
		this.#update(path);
	}

	/** Resolve the same most-specific-prefix and route preference as the origin. */
	instance(path: Path.Valid): Selection {
		for (const prefix of coveringPrefixes(path)) {
			const best = bestCandidate(path, prefix, this.#eligible(path, prefix), (entry) => entry.route);
			if (best) {
				const cached = this.#cache.get(path)?.get(best.route.instance ?? best.identity);
				// A minted instance names the announcement, not the publisher's stream, so a
				// request for it can only name the path: it holds while this route is selected.
				const named = best.route.instance !== undefined && !Instance.isMinted(best.route.instance);
				return {
					...best,
					validity: !named
						? ((cached?.consumer.closed.peek() === undefined ? cached?.selection.validity : undefined) ??
							new Validity())
						: undefined,
				};
			}
		}
		return this.#blind.get(path) ?? new Blind();
	}

	/**
	 * The best route of `instance` alone at `path`, whichever instance wins the path, as a
	 * request pinned to it resolves. With no route of it, the instance alone, which the peer
	 * resolves or refuses.
	 */
	pinned(path: Path.Valid, instance: Instance.Valid): Selection {
		for (const prefix of coveringPrefixes(path)) {
			const entries = this.#eligible(path, prefix).filter((entry) => entry.route.instance === instance);
			const best = bestCandidate(path, prefix, entries, (entry) => entry.route);
			if (best) return best;
		}
		return { identity: {}, route: { instance } };
	}

	/**
	 * The origin node of the best route now serving `instance` at `path` (lite-07), whichever
	 * instance wins the path, or undefined when no route names one.
	 */
	node(path: Path.Valid, instance: Instance.Valid): bigint | undefined {
		return this.pinned(path, instance).route.node;
	}

	/** Clone the live cached consumer only when it serves the selected instance. */
	get(path: Path.Valid, instance: Served): broadcast.Consumer | undefined {
		const cached = this.#cache.get(path)?.get(instance.route.instance ?? instance.identity);
		return cached?.consumer.closed.peek() === undefined ? cached?.consumer.clone() : undefined;
	}

	/** Cache each live instance so a temporary route winner never displaces a held fallback. */
	insert(path: Path.Valid, instance: Selection, consumer: broadcast.Consumer): broadcast.Consumer {
		let entries = this.#cache.get(path);
		if (!entries) {
			entries = new Map();
			this.#cache.set(path, entries);
		}
		const key = instance.route.instance ?? instance.identity;
		const entry = { consumer, selection: instance };
		entries.set(key, entry);
		for (const prefix of coveringPrefixes(path)) {
			let paths = this.#paths.get(prefix);
			if (!paths) {
				paths = new Set();
				this.#paths.set(prefix, paths);
			}
			paths.add(path);
		}
		if (instance instanceof Blind) {
			this.#blind.set(path, instance);
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
			instance.validity?.update(false);
			if (entries.get(key) !== entry) return;
			entries.delete(key);
			if (entries.size === 0 && this.#cache.get(path) === entries) this.#forgetPath(path);
			if (this.#blind.get(path) === instance) this.#forgetBlind(path);
		});
		return consumer;
	}

	#forgetBlind(path: Path.Valid): void {
		const blind = this.#blind.get(path);
		if (!blind) return;
		blind.validity.update(false);
		this.#blind.delete(path);
		const entries = this.#cache.get(path);
		entries?.delete(blind.identity);
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

	// The advertisements at `prefix` a consume of `path` may select, oldest first.
	#eligible(path: Path.Valid, prefix: Path.Valid): Advertised[] {
		const covering = this.#covering;
		return [...(this.#announced.get(prefix)?.values() ?? [])].filter(
			(entry) =>
				!entry.parked &&
				(prefix === path ||
					entry.route.node === undefined ||
					!covering ||
					covering.covers(entry.route.node, prefix, path)),
		);
	}

	#update(prefix: Path.Valid): void {
		for (const path of this.#paths.get(prefix) ?? []) this.#check(path);
	}

	#check(path: Path.Valid): void {
		const selected = this.instance(path);
		for (const entry of this.#cache.get(path)?.values() ?? []) {
			const same = sameInstance(entry.selection, selected);
			entry.selection.validity?.update(same, !same && !(selected instanceof Blind));
		}
	}
}
