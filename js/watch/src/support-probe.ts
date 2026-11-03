import { type Effect, type Getter, Signal } from "@moq/signals";

/** Internal key used to describe which fields can change a support probe's result. */
export const supportCacheKey = Symbol("supportCacheKey");

/** A function that checks if a rendition configuration can be played. */
type Supported<Config extends { codec: string }> = ((config: Config) => Promise<boolean>) & {
	[supportCacheKey]?: (config: Config) => string;
};

/**
 * Probes which renditions a `supported` function accepts, shared by the audio and video sources.
 *
 * A result is cached per rendition until the rendition's config changes, keyed by the probe's
 * `supportCacheKey` or else the whole config.
 */
export class SupportProbe<Config extends { codec: string }> {
	readonly #kind: string;

	readonly #pending = new Signal(false);
	/** Whether rendition support is still being resolved. */
	readonly pending: Getter<boolean> = this.#pending;

	/** A `NotSupportedError` once no rendition of a non-empty catalog is supported. */
	readonly error = new Signal<Error | undefined>(undefined);

	#cache = new WeakMap<Supported<Config>, Map<string, { key: string; supported: boolean }>>();

	constructor(kind: "audio" | "video") {
		this.#kind = kind;
	}

	/**
	 * Probe `renditions` within `effect`, then `publish` the supported ones.
	 * Nothing is published when the effect is torn down first, since its rerun redoes this.
	 */
	run(
		effect: Effect,
		supported: Supported<Config> | undefined,
		renditions: Record<string, Config>,
		publish: (available: Record<string, Config>) => void,
	): void {
		this.error.set(undefined);
		if (!supported) return;

		let cache = this.#cache.get(supported);
		if (!cache) {
			cache = new Map();
			this.#cache.set(supported, cache);
		}
		const names = new Set(Object.keys(renditions));
		for (const name of cache.keys()) {
			if (!names.has(name)) cache.delete(name);
		}

		const cacheKey = supported[supportCacheKey] ?? ((config: Config) => JSON.stringify(config));

		effect.set(this.#pending, true, false);
		effect.spawn(async () => {
			const available: Record<string, Config> = {};

			for (const [name, config] of Object.entries(renditions)) {
				const key = cacheKey(config);
				const cached = cache.get(name);
				let isSupported = cached?.key === key ? cached.supported : undefined;
				if (isSupported === undefined) {
					try {
						// `supported` comes from the consumer, so we cannot assume it ever settles. A
						// rerun waits for the tasks it spawned, so an unraced probe would hold it shut.
						const result = await effect.race(supported(config));
						// Undefined when torn down; the abort check below handles it.
						if (result !== undefined) {
							isSupported = !!result;
							// Recompute the key: a probe may rewrite the config it accepted.
							cache.set(name, { key: cacheKey(config), supported: isSupported });
						}
					} catch (err) {
						if (effect.abort.aborted) return;
						console.warn(
							`[Source] ${this.#kind} rendition ${name} (${config.codec}) support probe failed; treating as unsupported`,
							err,
						);
					}
				}

				// Torn down: stop probing and publish nothing, since the rerun redoes this.
				if (effect.abort.aborted) return;

				if (isSupported) available[name] = config;
			}

			if (Object.keys(available).length === 0 && Object.keys(renditions).length > 0) {
				console.warn(`[Source] no supported ${this.#kind} renditions found:`, renditions);
				this.error.set(new DOMException(`no supported ${this.#kind} renditions`, "NotSupportedError"));
			}
			publish(available);
			this.#pending.set(false);
		});
	}
}
