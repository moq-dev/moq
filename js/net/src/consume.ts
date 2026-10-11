import type * as broadcast from "./broadcast.ts";
import type * as Epoch from "./epoch.ts";
import type { Route } from "./hop.ts";
import { compareRouteCandidates, coveringPrefixes } from "./internal.ts";
import type * as Path from "./path.ts";
import type { Instance } from "./wire.ts";

class Blind implements Instance {
	readonly identity = {};
	readonly route = {};
}

/** Consumed broadcasts shared only within the selected serving instance. @internal */
export class BroadcastCache {
	#cache = new Map<Path.Valid, Map<Epoch.Valid | object, broadcast.Consumer>>();
	#announced = new Map<Path.Valid, Map<object, { identity: object; route: Route }>>();
	#blind = new Map<Path.Valid, Blind>();
	#blindPrefixes = new Map<Path.Valid, Set<Path.Valid>>();

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
	}

	/** Withdraw this interest's advertisement without disturbing another interest's routes. */
	withdraw(path: Path.Valid, owner: object): void {
		const entries = this.#announced.get(path);
		entries?.delete(owner);
		if (entries?.size === 0) this.#announced.delete(path);
	}

	/** Resolve the same most-specific-prefix and route preference as the origin. */
	instance(path: Path.Valid): Instance {
		for (const prefix of coveringPrefixes(path)) {
			const entries = this.#announced.get(prefix);
			if (!entries) continue;
			let best: { identity: object; route: Route } | undefined;
			for (const entry of entries.values()) {
				if (!best || compareRouteCandidates(path, entry.route, best.route) < 0) best = entry;
			}
			if (best) return best;
		}
		return this.#blind.get(path) ?? new Blind();
	}

	/** Clone the live cached consumer only when it serves the selected instance. */
	get(path: Path.Valid, instance: Instance): broadcast.Consumer | undefined {
		const cached = this.#cache.get(path)?.get(instance.route.epoch ?? instance.identity);
		return cached?.closed.peek() === undefined ? cached?.clone() : undefined;
	}

	/** Cache each live instance so a temporary route winner never displaces a held fallback. */
	insert(path: Path.Valid, instance: Instance, consumer: broadcast.Consumer): broadcast.Consumer {
		let entries = this.#cache.get(path);
		if (!entries) {
			entries = new Map();
			this.#cache.set(path, entries);
		}
		const key = instance.route.epoch ?? instance.identity;
		entries.set(key, consumer);
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
			if (entries.get(key) !== consumer) return;
			entries.delete(key);
			if (entries.size === 0 && this.#cache.get(path) === entries) this.#cache.delete(path);
			if (this.#blind.get(path) === instance) this.#forgetBlind(path);
		});
		return consumer;
	}

	#forgetBlind(path: Path.Valid): void {
		const blind = this.#blind.get(path);
		if (!blind) return;
		this.#blind.delete(path);
		const entries = this.#cache.get(path);
		entries?.delete(blind.identity);
		if (entries?.size === 0) this.#cache.delete(path);
		for (const prefix of coveringPrefixes(path)) {
			const paths = this.#blindPrefixes.get(prefix);
			paths?.delete(path);
			if (paths?.size === 0) this.#blindPrefixes.delete(prefix);
		}
	}
}
