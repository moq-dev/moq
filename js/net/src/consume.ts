import type * as broadcast from "./broadcast.ts";
import type * as Path from "./path.ts";

/**
 * Per-path dedup cache for consumed broadcasts, shared by the moq-lite and moq-ietf
 * subscribers.
 *
 * `Connection.consume(path)` must not mint a fresh subscription per call: repeat requests
 * for the same path (e.g. several renditions referencing one `broadcast: "../source"`) should
 * share a single upstream subscription. This mirrors the Rust `origin::Consumer` weak-cache:
 * a still-live path resolves to a shared {@link broadcast.Consumer.clone}, a closed one is
 * re-consumed on the next request. Each handle is reference-counted, so the shared broadcast
 * closes once every caller has closed its handle.
 *
 * @internal
 */
export class BroadcastCache {
	// The base handle per path; callers get reference-counted clones of it.
	#cache = new Map<Path.Valid, broadcast.Consumer>();

	/** A shared handle to the live broadcast cached for `path`, or `undefined` on a miss. */
	get(path: Path.Valid): broadcast.Consumer | undefined {
		const base = this.#cache.get(path);
		if (base && base.closed.peek() === undefined) return base.clone();
		return undefined;
	}

	/**
	 * Cache `consumer` as the base handle for `path` (evicting it once it closes) and return it.
	 * Call on a {@link get} miss, after wiring up the fresh consumer's subscribe loop.
	 */
	insert(path: Path.Valid, consumer: broadcast.Consumer): broadcast.Consumer {
		this.#cache.set(path, consumer);

		// Drop the entry once the broadcast closes (every handle released), so the next request
		// re-consumes rather than cloning a dead handle. Guard against a newer entry for the path.
		void consumer.closed.then(() => {
			if (this.#cache.get(path) === consumer) this.#cache.delete(path);
		});

		return consumer;
	}
}
