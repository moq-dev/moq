import type * as broadcast from "./broadcast.ts";
import type * as Epoch from "./epoch.ts";
import { StreamCode, StreamError } from "./error.ts";
import type { Route } from "./hop.ts";
import { compareRouteCandidates, coveringPrefixes } from "./internal.ts";
import type * as Path from "./path.ts";
import { type Instance, sameInstance } from "./wire.ts";

class Validity {
	#current = new AbortController();

	get signal(): AbortSignal {
		return this.#current.signal;
	}

	update(allowed: boolean): void {
		if (allowed) {
			if (this.#current.signal.aborted) this.#current = new AbortController();
		} else if (!this.#current.signal.aborted) {
			// Unroutable, as in Rust: the instance going away is an end, not a failure.
			this.#current.abort(new StreamError(StreamCode.Unroutable, { message: "broadcast instance retired" }));
		}
	}
}

type Selection = Instance & { readonly validity?: Validity };

class Blind implements Selection {
	readonly identity = {};
	readonly route = {};
	readonly validity = new Validity();
}

// private: plumbing
/** Consumed broadcasts shared only within the selected serving instance. @internal */
export class BroadcastCache {
	#cache = new Map<Path.Valid, Map<Epoch.Valid | object, { consumer: broadcast.Consumer; selection: Selection }>>();
	#announced = new Map<Path.Valid, Map<object, { identity: object; route: Route }>>();
	#blind = new Map<Path.Valid, Blind>();
	#blindPrefixes = new Map<Path.Valid, Set<Path.Valid>>();
	#paths = new Map<Path.Valid, Set<Path.Valid>>();

	/** Record this interest's live advertisement, preserving identity across route updates. */
	announce(path: Path.Valid, owner: object, route: Route, restart = false): void {
		// Only unannounced consumers covered by this advertisement lose their blind identity.
		for (const covered of this.#blindPrefixes.get(path) ?? []) this.#forgetBlind(covered);
		let entries = this.#announced.get(path);
		if (!entries) {
			entries = new Map();
			this.#announced.set(path, entries);
		}
		const identity = (!restart && entries.get(owner)?.identity) || {};
		entries.set(owner, { identity, route });
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
	instance(path: Path.Valid): Selection {
		for (const prefix of coveringPrefixes(path)) {
			const entries = this.#announced.get(prefix);
			if (!entries) continue;
			let best: { identity: object; route: Route } | undefined;
			for (const entry of entries.values()) {
				if (!best || compareRouteCandidates(path, prefix, entry.route, best.route) < 0) best = entry;
			}
			if (best) {
				const cached = this.#cache.get(path)?.get(best.route.epoch ?? best.identity);
				return {
					...best,
					validity:
						best.route.epoch === undefined
							? ((cached?.consumer.closed.peek() === undefined
									? cached?.selection.validity
									: undefined) ?? new Validity())
							: undefined,
				};
			}
		}
		return this.#blind.get(path) ?? new Blind();
	}

	/** Clone the live cached consumer only when it serves the selected instance. */
	get(path: Path.Valid, instance: Instance): broadcast.Consumer | undefined {
		const cached = this.#cache.get(path)?.get(instance.route.epoch ?? instance.identity);
		return cached?.consumer.closed.peek() === undefined ? cached?.consumer.clone() : undefined;
	}

	/** Cache each live instance so a temporary route winner never displaces a held fallback. */
	insert(path: Path.Valid, instance: Selection, consumer: broadcast.Consumer): broadcast.Consumer {
		let entries = this.#cache.get(path);
		if (!entries) {
			entries = new Map();
			this.#cache.set(path, entries);
		}
		const key = instance.route.epoch ?? instance.identity;
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

	#update(prefix: Path.Valid): void {
		for (const path of this.#paths.get(prefix) ?? []) {
			const selected = this.instance(path);
			for (const entry of this.#cache.get(path)?.values() ?? []) {
				entry.selection.validity?.update(sameInstance(entry.selection, selected));
			}
		}
	}
}
