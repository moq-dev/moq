import { type Effect, type Getter, Signal } from "@moq/signals";

/** How a decoder answers whether it can play a rendition, and which config fields change that answer. */
export interface Probe<Config> {
	/** Whether the decoder can play `config`. */
	supported(config: Config): Promise<boolean>;
	/** A key equal for every config `supported` answers the same. */
	key(config: Config): string;
}

/**
 * Probes which renditions a decoder can play, shared by the audio and video sources.
 *
 * A result is cached per rendition until the probe's key for its config changes.
 */
export class SupportProbe<Config extends { codec: string }> {
	readonly #kind: string;
	readonly #probe: Probe<Config>;

	readonly #pending = new Signal(false);
	/** Whether rendition support is still being resolved. */
	readonly pending: Getter<boolean> = this.#pending;

	/** A `NotSupportedError` once no rendition of a non-empty catalog is supported. */
	readonly error = new Signal<Error | undefined>(undefined);

	readonly #cache = new Map<string, { key: string; supported: boolean }>();

	constructor(kind: "audio" | "video", probe: Probe<Config>) {
		this.#kind = kind;
		this.#probe = probe;
	}

	/**
	 * Probe `renditions` within `effect`, then `publish` the supported ones.
	 * Nothing is published when the effect is torn down first, since its rerun redoes this.
	 */
	run(
		effect: Effect,
		renditions: Record<string, Config>,
		publish: (available: Record<string, Config>) => void,
	): void {
		this.error.set(undefined);

		const cache = this.#cache;
		const probe = this.#probe;
		const names = new Set(Object.keys(renditions));
		for (const name of cache.keys()) {
			if (!names.has(name)) cache.delete(name);
		}

		effect.set(this.#pending, true, false);
		effect.spawn(async () => {
			// A Map, since assigning an ID like `__proto__` into a plain object sets its prototype instead.
			const available = new Map<string, Config>();

			for (const [name, config] of Object.entries(renditions)) {
				const key = probe.key(config);
				const cached = cache.get(name);
				let isSupported = cached?.key === key ? cached.supported : undefined;
				if (isSupported === undefined) {
					try {
						// A decoder's probe need not ever settle. A rerun waits for the tasks it
						// spawned, so an unraced probe would hold it shut.
						const result = await effect.race(probe.supported(config));
						// Undefined when torn down; the abort check below handles it.
						if (result !== undefined) {
							isSupported = !!result;
							cache.set(name, { key, supported: isSupported });
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

				if (isSupported) available.set(name, config);
			}

			if (available.size === 0 && Object.keys(renditions).length > 0) {
				console.warn(`[Source] no supported ${this.#kind} renditions found:`, renditions);
				this.error.set(new DOMException(`no supported ${this.#kind} renditions`, "NotSupportedError"));
			}
			publish(Object.fromEntries(available));
			this.#pending.set(false);
		});
	}
}
