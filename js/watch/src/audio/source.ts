import { Milli } from "@moq/clock";
import type * as Catalog from "@moq/hang/catalog";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { type CacheableSupported, supportCacheKey } from "../support-cache";

/** What the viewer wants from the audio rendition. */
export type Target = {
	// Optional manual override for the selected rendition name.
	name?: string;
};

/**
 * A function that checks if an audio configuration can be played.
 *
 * `Decoder.supported` is the WebCodecs probe used by `<moq-watch>`.
 */
export type Supported = (config: Catalog.AudioConfig) => Promise<boolean>;

/** Signals the source reads. */
export type SourceInput = {
	broadcast: Getter<Broadcast | undefined>;

	// The desired rendition/bitrate of the audio.
	target: Getter<Target | undefined>;

	// A function that checks if an audio configuration can be played. Renditions that fail the
	// probe are filtered out. Nothing is selected until one is provided.
	supported: Getter<Supported | undefined>;
};

/** Signals the source derives. */
export type SourceOutput = {
	catalog: Signal<Catalog.Audio | undefined>;
	available: Signal<Record<string, Catalog.AudioConfig>>;
	track: Signal<string | undefined>;
	config: Signal<Catalog.AudioConfig | undefined>;

	// The jitter the selected rendition advertises (ms): a floor on the decoder's playout target.
	jitter: Signal<Milli | undefined>;
};

/**
 * Source handles catalog extraction, support checking, and rendition selection
 * for audio playback. The Decoder consumes whichever rendition it picks.
 */
export class Source {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<SourceInput>;

	readonly #out: SourceOutput = {
		catalog: new Signal<Catalog.Audio | undefined>(undefined),
		available: new Signal<Record<string, Catalog.AudioConfig>>({}),
		track: new Signal<string | undefined>(undefined),
		config: new Signal<Catalog.AudioConfig | undefined>(undefined),
		jitter: new Signal<Milli | undefined>(undefined),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);
	#pending = new Signal(false);
	/** Whether rendition support is still being resolved. @internal */
	readonly pending: Getter<boolean> = this.#pending;

	#signals = new Effect();
	#supportCache = new WeakMap<Supported, Map<string, { key: string; supported: boolean }>>();

	constructor(props?: Inputs<SourceInput>) {
		this.in = {
			broadcast: getter(props?.broadcast),
			target: getter(props?.target),
			supported: getter(props?.supported),
		};

		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runSupported.bind(this));
		this.#signals.run(this.#runSelected.bind(this));
	}

	#runCatalog(effect: Effect): void {
		const broadcast = effect.get(this.in.broadcast);
		if (!broadcast) return;

		const catalog = effect.get(broadcast.out.catalog)?.audio;
		if (!catalog) return;

		effect.set(this.#out.catalog, catalog);
	}

	#runSupported(effect: Effect): void {
		const renditions = effect.get(this.#out.catalog)?.renditions ?? {};
		const supported = effect.get(this.in.supported);
		if (!supported) return;

		let cache = this.#supportCache.get(supported);
		if (!cache) {
			cache = new Map();
			this.#supportCache.set(supported, cache);
		}
		// Retain only the latest config per current rendition, not a catalog's history.
		const names = new Set(Object.keys(renditions));
		for (const name of cache.keys()) {
			if (!names.has(name)) cache.delete(name);
		}
		const cacheKey = (supported as CacheableSupported<Catalog.AudioConfig>)[supportCacheKey];

		effect.set(this.#pending, true, false);
		effect.spawn(async () => {
			try {
				const available: Record<string, Catalog.AudioConfig> = {};

				for (const [name, config] of Object.entries(renditions)) {
					const key = cacheKey ? cacheKey(config) : JSON.stringify(config);
					const cached = cache.get(name);
					let isSupported = cached?.key === key ? cached.supported : undefined;
					if (isSupported === undefined) {
						try {
							// A consumer's probe may never settle; teardown must still release this run.
							isSupported = await effect.race(supported(config));
						} catch (err) {
							if (effect.abort.aborted) return;
							console.warn(
								`[Source] audio rendition ${name} (${config.codec}) support probe failed; treating as unsupported`,
								err,
							);
						}

						// A canceled or rejected probe supplies no verdict for the next run.
						if (effect.abort.aborted) return;
						if (isSupported !== undefined) cache.set(name, { key, supported: isSupported });
					}

					if (isSupported) available[name] = config;
				}

				if (Object.keys(available).length === 0 && Object.keys(renditions).length > 0) {
					console.warn("no supported audio renditions found:", renditions);
				}

				this.#out.available.set(available);
			} finally {
				this.#pending.set(false);
			}
		});
	}

	#runSelected(effect: Effect): void {
		// A disabled rendition has no frames coming, so it is never selected, even by name.
		const available = Object.fromEntries(
			Object.entries(effect.get(this.#out.available)).filter(([, config]) => config.enabled !== false),
		);
		if (Object.keys(available).length === 0) return;

		const target = effect.get(this.in.target);

		let selected: { track: string; config: Catalog.AudioConfig } | undefined;

		// Manual selection by name
		if (target?.name && target.name in available) {
			selected = { track: target.name, config: available[target.name] };
		} else {
			// Automatic selection
			selected = this.#select(available);
			if (!selected) return;
		}

		effect.set(this.#out.track, selected.track);
		effect.set(this.#out.config, selected.config);

		const jitter = selected.config.jitter;
		effect.set(this.#out.jitter, jitter !== undefined ? Milli(jitter) : undefined);
	}

	/**
	 * Select rendition based on the configured strategy.
	 */
	#select(
		renditions: Record<string, Catalog.AudioConfig>,
	): { track: string; config: Catalog.AudioConfig } | undefined {
		const entries = Object.entries(renditions);
		if (entries.length === 0) return undefined;

		for (const [track, config] of entries) {
			if (config.container.kind === "legacy") {
				return { track, config };
			}
		}

		for (const [track, config] of entries) {
			if (config.container.kind === "loc") {
				return { track, config };
			}
		}

		for (const [track, config] of entries) {
			if (config.container.kind === "cmaf") {
				return { track, config };
			}
		}

		return undefined;
	}

	/** Stop and release everything this holds. */
	close(): void {
		this.#signals.close();
		this.#supportCache = new WeakMap();
	}
}
