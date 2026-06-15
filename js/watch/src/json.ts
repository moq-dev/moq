import * as Catalog from "@moq/hang/catalog";
import * as Json from "@moq/json";
import type * as Moq from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";

/** Options for {@link Broadcast.subscribeJson}, extending the {@link Json.Config} for the value. */
export type SubscribeJsonOptions<T> = Json.Config<T> & {
	/**
	 * Subscription priority. Defaults to {@link Catalog.PRIORITY.catalog} so metadata arrives ahead
	 * of media.
	 */
	priority?: number;
};

/**
 * Consumes a custom JSON track from a broadcast, following the active broadcast across reconnects.
 *
 * The latest reconstructed value is exposed as a {@link Signal} via {@link value}. Call
 * {@link close} when done (or let the owning watch {@link Broadcast} close it).
 */
export class JsonConsumer<T> {
	/** The latest reconstructed value, or `undefined` before the first frame or while offline. */
	readonly value = new Signal<T | undefined>(undefined);

	#signals = new Effect();

	/** Subscribe to `name` on whatever broadcast `active` currently holds. */
	constructor(active: Getter<Moq.Broadcast | undefined>, name: string, options?: SubscribeJsonOptions<T>) {
		const priority = options?.priority ?? Catalog.PRIORITY.catalog;

		this.#signals.run((effect) => {
			const broadcast = effect.get(active);
			if (!broadcast) return;

			const track = broadcast.subscribe(name, priority);
			effect.cleanup(() => track.close());

			// Clear the value when this subscription tears down so a stale value from a previous
			// broadcast doesn't linger after a reconnect.
			effect.cleanup(() => this.value.set(undefined));

			const consumer = new Json.Consumer<T>(track, options);
			effect.spawn(async () => {
				try {
					for (;;) {
						const next = await Promise.race([effect.cancel, consumer.next()]);
						if (next === undefined) break;
						this.value.set(next);
					}
				} catch (err) {
					console.warn("error fetching json track", name, err);
				}
			});
		});
	}

	/** Stop subscribing and release resources. */
	close(): void {
		this.#signals.close();
	}
}
