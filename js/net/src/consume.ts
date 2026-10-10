import type * as broadcast from "./broadcast.ts";
import type { Route } from "./hop.ts";
import { compareRouteCandidates, coveringPrefixes } from "./internal.ts";
import type * as Path from "./path.ts";
import { type Instance, sameInstance } from "./wire.ts";

/** Consumed broadcasts shared only within the selected serving instance. @internal */
export class BroadcastCache {
	#cache = new Map<Path.Valid, { consumer: broadcast.Consumer; instance: Instance }>();
	#announced = new Map<Path.Valid, Map<object, { identity: object; route: Route }>>();
	#blind: Instance = { identity: {}, route: {} };

	/** Record this interest's live advertisement, preserving identity across route updates. */
	announce(path: Path.Valid, owner: object, route: Route, restart = false): void {
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
		return this.#blind;
	}

	/** Clone the live cached consumer only when it serves the selected instance. */
	get(path: Path.Valid, instance: Instance): broadcast.Consumer | undefined {
		const cached = this.#cache.get(path);
		if (cached && cached.consumer.closed.peek() === undefined && sameInstance(cached.instance, instance)) {
			return cached.consumer.clone();
		}
		return undefined;
	}

	/** Cache a new consumer without closing handles pinned to the previous instance. */
	insert(path: Path.Valid, instance: Instance, consumer: broadcast.Consumer): broadcast.Consumer {
		const entry = { instance, consumer };
		this.#cache.set(path, entry);
		void consumer.closed.then(() => {
			if (this.#cache.get(path) === entry) this.#cache.delete(path);
		});
		return consumer;
	}
}
