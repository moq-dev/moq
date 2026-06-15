import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import type { Effect } from "@moq/signals";

/**
 * A stable JSON value that fans out to on-demand subscription tracks.
 *
 * Unlike a raw {@link Json.Producer}, this exists independently of any subscription: set the value
 * at any time with {@link update} or {@link mutate}, and each subscriber (including a relay that
 * reconnects) is seeded with the current value before receiving updates. Multiple independent owners
 * can share one instance and each edit only their own keys via {@link mutate}, so their sections
 * compose instead of clobbering one another.
 *
 * This backs both the broadcast catalog and arbitrary custom JSON tracks (see
 * `Broadcast.publishJson`).
 */
export class JsonProducer<T> {
	#value: T | undefined;
	#outputs = new Set<Json.Producer<T>>();
	#config: Json.Config<T>;

	/** Create a producer, optionally seeding an initial value and per-track config. */
	constructor(config: Json.Config<T> = {}) {
		this.#config = config;
		this.#value = config.initial;
	}

	/** The current value, or `undefined` if nothing has been published yet. */
	get value(): T | undefined {
		return this.#value;
	}

	/** Replace the value; the result is published to all current subscribers. No-op if unchanged. */
	update(value: T): void {
		this.#value = value;
		for (const output of this.#outputs) output.update(value);
	}

	/**
	 * Mutate the current value in place and publish the result.
	 *
	 * The callback receives a deep clone of the last value (falling back to the configured `initial`,
	 * throwing if neither exists). Edit it in place; on return the result is published via
	 * {@link update}.
	 */
	mutate(fn: (value: T) => void): void {
		const base = this.#value ?? this.#config.initial;
		if (base === undefined) {
			throw new Error("mutate() requires a prior update() or `initial` in the config");
		}

		const value = structuredClone(base) as T;
		fn(value);
		this.update(value);
	}

	/** Serve a subscription request: seed it with the current value, then forward updates. */
	serve(track: Moq.Track, effect: Effect): void {
		const output = new Json.Producer<T>(track, this.#config);
		if (this.#value !== undefined) output.update(this.#value);

		this.#outputs.add(output);
		effect.cleanup(() => {
			this.#outputs.delete(output);
			output.finish();
		});
	}
}
