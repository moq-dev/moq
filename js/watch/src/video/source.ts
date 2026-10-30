import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { supportCacheKey } from "./config";

/**
 * A function that checks if a video configuration can be played.
 *
 * `Decoder.supported` is the WebCodecs probe used by `<moq-watch>`.
 */
export type Supported = (config: Catalog.VideoConfig) => Promise<boolean>;

type CacheableSupported = Supported & {
	[supportCacheKey]?: (config: Catalog.VideoConfig) => string;
};

/** What the viewer wants from the video rendition; the source picks the best one within it. */
export type Target = {
	// Optional manual override for the selected rendition name.
	name?: string;

	// Maximum desired pixel area (codedWidth * codedHeight).
	pixels?: number;

	// Maximum desired coded width in pixels.
	width?: number;

	// Maximum desired coded height in pixels.
	height?: number;

	// Maximum desired bitrate in bits per second.
	bitrate?: number;
};

/** Signals the source reads. */
export type SourceInput = {
	broadcast: Getter<Broadcast | undefined>;
	target: Getter<Target | undefined>;

	// A function that checks if a video configuration can be played. Renditions that fail the
	// probe are filtered out. Nothing is selected until one is provided.
	supported: Getter<Supported | undefined>;

	// The connection's PROBE estimates, used to auto-select a rendition when the target has no
	// explicit bitrate. Usually wired from a `Connection`'s `probe`.
	// Optional: without it auto-selection falls back to the rank order alone.
	probe: Getter<Moq.Connection.Probe | undefined>;
};

/** Signals the source derives. */
export type SourceOutput = {
	catalog: Signal<Catalog.Video | undefined>;
	available: Signal<Record<string, Catalog.VideoConfig>>;

	// Why no rendition can be chosen, or undefined while healthy or still probing. A
	// `NotSupportedError` means the browser can decode none of the catalog's renditions.
	error: Signal<Error | undefined>;

	// The name of the active rendition.
	track: Signal<string | undefined>;
	config: Signal<Catalog.VideoConfig | undefined>;
};

/**
 * A filter that returns matching renditions sorted by preference (most preferred first).
 * Must return at least one rendition.
 */
type RenditionFilter = (entries: [string, Catalog.VideoConfig][]) => string[];

/**
 * Filter and rank renditions by a maximum pixel count.
 * Returns renditions within budget (largest first for best quality).
 * Over-budget and unknown-resolution renditions are excluded.
 * If nothing is within budget, falls back to the single smallest rendition.
 */
function byPixels(target: number): RenditionFilter {
	return (entries) => {
		const within: { name: string; size: number }[] = [];
		const rest: { name: string; size: number }[] = [];

		for (const [name, config] of entries) {
			if (config.codedWidth && config.codedHeight) {
				const size = config.codedWidth * config.codedHeight;
				if (size <= target) {
					within.push({ name, size });
				} else {
					rest.push({ name, size });
				}
			}
		}

		// Best quality within budget
		within.sort((a, b) => b.size - a.size);

		if (within.length > 0) {
			return within.map((e) => e.name);
		}

		// Degrade to smallest over-budget resolution.
		if (rest.length > 0) {
			rest.sort((a, b) => a.size - b.size);
			return [rest[0].name];
		}

		// No entries had resolution metadata, so return all names unranked.
		return entries.map(([name]) => name);
	};
}

/**
 * Filter and rank renditions by maximum coded dimensions.
 * Returns renditions where codedWidth <= width AND codedHeight <= height
 * (each cap is optional). Within-budget renditions rank by area (largest first).
 * If nothing fits, falls back to the single smallest over-budget rendition.
 */
function byDimensions(width?: number, height?: number): RenditionFilter {
	return (entries) => {
		const within: { name: string; size: number }[] = [];
		const rest: { name: string; size: number }[] = [];

		for (const [name, config] of entries) {
			if (!config.codedWidth || !config.codedHeight) continue;
			const size = config.codedWidth * config.codedHeight;
			const fitsWidth = width == null || config.codedWidth <= width;
			const fitsHeight = height == null || config.codedHeight <= height;
			if (fitsWidth && fitsHeight) {
				within.push({ name, size });
			} else {
				rest.push({ name, size });
			}
		}

		// Best quality within budget
		within.sort((a, b) => b.size - a.size);

		if (within.length > 0) {
			return within.map((e) => e.name);
		}

		// Degrade to smallest over-budget rendition.
		if (rest.length > 0) {
			rest.sort((a, b) => a.size - b.size);
			return [rest[0].name];
		}

		// No entries had resolution metadata, so return all names unranked.
		return entries.map(([name]) => name);
	};
}

/**
 * Filter and rank renditions by a maximum bitrate budget.
 * Returns renditions within budget (highest bitrate first for best quality).
 * Over-budget and unknown-bitrate renditions are excluded.
 * If nothing is within budget, falls back to the single lowest-bitrate rendition.
 */
function byBitrate(target: number): RenditionFilter {
	return (entries) => {
		const within: { name: string; bitrate: number }[] = [];
		const rest: { name: string; bitrate: number }[] = [];

		for (const [name, config] of entries) {
			if (config.bitrate != null && config.bitrate <= target) {
				within.push({ name, bitrate: config.bitrate });
			} else if (config.bitrate != null) {
				rest.push({ name, bitrate: config.bitrate });
			}
		}

		// Best quality within budget
		within.sort((a, b) => b.bitrate - a.bitrate);

		if (within.length > 0) {
			return within.map((e) => e.name);
		}

		// Degrade to lowest over-budget bitrate.
		if (rest.length > 0) {
			rest.sort((a, b) => a.bitrate - b.bitrate);
			return [rest[0].name];
		}

		// No entries had bitrate metadata, so return all names unranked.
		return entries.map(([name]) => name);
	};
}

/** Return the renditions that may be newly selected: a disabled one has no frames coming. */
function enabledRenditions(renditions: Record<string, Catalog.VideoConfig>): Record<string, Catalog.VideoConfig> {
	return Object.fromEntries(Object.entries(renditions).filter(([, config]) => config.enabled !== false));
}

/** Keep only the highest preference: a lower one is a fallback for players that can't decode it. */
function preferredRenditions(renditions: Record<string, Catalog.VideoConfig>): Record<string, Catalog.VideoConfig> {
	const entries = Object.entries(renditions);
	// A loop, not `Math.max(...)`: a catalog's rendition count can exceed the argument limit.
	const top = entries.reduce((top, [, config]) => Math.max(top, config.preference ?? 0), Number.NEGATIVE_INFINITY);
	return Object.fromEntries(entries.filter(([, config]) => (config.preference ?? 0) === top));
}

/**
 * Source handles catalog extraction, support checking, and rendition selection
 * for video playback. The Decoder consumes whichever rendition it picks.
 */
export class Source {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<SourceInput>;

	readonly #out: SourceOutput = {
		catalog: new Signal<Catalog.Video | undefined>(undefined),
		available: new Signal<Record<string, Catalog.VideoConfig>>({}),
		error: new Signal<Error | undefined>(undefined),
		track: new Signal<string | undefined>(undefined),
		config: new Signal<Catalog.VideoConfig | undefined>(undefined),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);
	#pending = new Signal(false);
	/** Whether rendition support is still being resolved. @internal */
	readonly pending: Getter<boolean> = this.#pending;

	// The last selection, which a disable holds while no enabled rendition remains.
	#held?: { broadcast: Broadcast; track: string };

	// The broadcast `available` was probed for. It lags a broadcast switch until the new probe lands.
	// A signal, since the new broadcast's renditions can equal the old ones and leave `available` unchanged.
	#probed = new Signal<Broadcast | undefined>(undefined);

	#signals = new Effect();
	#supportCache = new WeakMap<Supported, Map<string, { key: string; supported: boolean }>>();

	constructor(props?: Inputs<SourceInput>) {
		this.in = {
			broadcast: getter(props?.broadcast),
			target: getter(props?.target),
			supported: getter(props?.supported),
			probe: getter(props?.probe),
		};

		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runSupported.bind(this));
		this.#signals.run(this.#runSelected.bind(this));
	}

	#runCatalog(effect: Effect): void {
		const broadcast = effect.get(this.in.broadcast);
		if (!broadcast) return;

		const catalog = effect.get(broadcast.out.catalog)?.video;
		if (!catalog) return;

		effect.set(this.#out.catalog, catalog);
	}

	#runSupported(effect: Effect): void {
		const supported = effect.get(this.in.supported);
		if (!supported) {
			this.#out.error.set(undefined);
			return;
		}

		// Read the catalog from the broadcast itself, so the probe result is tagged with the right one.
		const broadcast = effect.get(this.in.broadcast);
		const renditions = (broadcast && effect.get(broadcast.out.catalog)?.video?.renditions) ?? {};
		this.#out.error.set(undefined);
		let cache = this.#supportCache.get(supported);
		if (!cache) {
			cache = new Map();
			this.#supportCache.set(supported, cache);
		}
		const names = new Set(Object.keys(renditions));
		for (const name of cache.keys()) {
			if (!names.has(name)) cache.delete(name);
		}

		effect.set(this.#pending, true, false);
		effect.spawn(async () => {
			const available: Record<string, Catalog.VideoConfig> = {};

			for (const [name, config] of Object.entries(renditions)) {
				const cacheKey = (supported as CacheableSupported)[supportCacheKey];
				const key = cacheKey ? cacheKey(config) : JSON.stringify(config);
				const cached = cache.get(name);
				let isSupported: boolean | undefined = false;
				if (cached?.key === key) {
					isSupported = cached.supported;
				} else {
					let failed = false;
					try {
						// `supported` comes from the consumer, so we cannot assume it ever settles. A
						// rerun waits for the tasks it spawned, so an unraced probe would hold it shut.
						isSupported = await effect.race(supported(config));
					} catch (err) {
						failed = true;
						console.warn(
							`[Source] video rendition ${name} (${config.codec}) support probe failed; treating as unsupported`,
							err,
						);
					}
					if (!failed && isSupported !== undefined) {
						cache.set(name, {
							key: cacheKey ? cacheKey(config) : JSON.stringify(config),
							supported: isSupported,
						});
					}
				}

				// Torn down: stop probing and publish nothing, since the rerun redoes this.
				if (effect.abort.aborted) return;

				if (isSupported) available[name] = config;
			}

			if (Object.keys(available).length === 0 && Object.keys(renditions).length > 0) {
				console.warn("[Source] No supported video renditions found:", renditions);
				this.#out.error.set(new DOMException("no supported video renditions", "NotSupportedError"));
			} else {
				this.#out.error.set(undefined);
			}
			this.#probed.set(broadcast);
			this.#out.available.set(available);
			this.#pending.set(false);
		});
	}

	#runSelected(effect: Effect): void {
		const broadcast = effect.get(this.in.broadcast);
		const all = effect.get(this.#out.available);
		const probed = effect.get(this.#probed);
		const available = enabledRenditions(all);
		const target = effect.get(this.in.target);

		const selected = this.#choose(effect, available, target) ?? this.#hold(broadcast, all);
		// A pick from the previous broadcast's renditions is not this broadcast's selection to hold.
		this.#held = selected && broadcast && probed === broadcast ? { broadcast, track: selected } : undefined;
		if (!selected) return;

		effect.set(this.#out.track, selected);
		effect.set(this.#out.config, all[selected]);
	}

	// Pick among the enabled renditions.
	#choose(effect: Effect, available: Record<string, Catalog.VideoConfig>, target?: Target): string | undefined {
		if (target?.name && target.name in available) return target.name;
		if (Object.keys(available).length === 0) return undefined;

		// Auto-select: use recv bandwidth if no explicit bitrate target.
		let effectiveTarget = target;
		if (!target?.bitrate) {
			const estimate = effect.get(this.in.probe)?.estimatedRecvRate;
			if (estimate != null) {
				// Apply a safety margin (80%) to avoid oscillation.
				const safeBitrate = Math.round(estimate * 0.8);
				effectiveTarget = { ...target, bitrate: safeBitrate };
			}
		}

		return this.#select(preferredRenditions(available), effectiveTarget);
	}

	// Every rendition is disabled: keep the selected one, and its subscription, so the re-enable
	// plays without a resubscribe. A removed rendition, or another broadcast, starts over.
	#hold(broadcast: Broadcast | undefined, all: Record<string, Catalog.VideoConfig>): string | undefined {
		if (Object.values(all).some((config) => config.enabled !== false)) return undefined;
		const held = this.#held;
		if (!held || held.broadcast !== broadcast || !(held.track in all)) return undefined;
		return held.track;
	}

	/**
	 * Select the best rendition using a generic filter system.
	 *
	 * Each enabled filter returns matching renditions sorted by preference.
	 * The first rendition present in every filter's output is selected.
	 * If the rankings conflict, the first ranking's best rendition is selected.
	 */
	#select(renditions: Record<string, Catalog.VideoConfig>, target?: Target): string | undefined {
		const entries = Object.entries(renditions);
		if (entries.length === 0) return undefined;
		if (entries.length === 1) return entries[0][0];

		// Build enabled filters based on the target.
		const filters: RenditionFilter[] = [];

		if (target?.pixels != null) {
			filters.push(byPixels(target.pixels));
		}
		if (target?.width != null || target?.height != null) {
			filters.push(byDimensions(target.width, target.height));
		}
		if (target?.bitrate != null) {
			filters.push(byBitrate(target.bitrate));
		}

		// No cap: the same rank as Rust's single-rendition egresses.
		if (filters.length === 0) {
			return Catalog.Video.ranked(renditions)[0][0];
		}

		// Run each filter to get ranked preference lists.
		const rankings = filters.map((f) => f(entries));

		// Select the first rendition (in the first ranking's order) present in all rankings.
		const sets = rankings.map((r) => new Set(r));

		for (const name of rankings[0]) {
			if (sets.every((s) => s.has(name))) {
				return name;
			}
		}

		console.warn("conflicting rendition filters, using the first ranking's preferred rendition");
		return rankings[0][0];
	}

	/** Stop and release everything this holds. */
	close(): void {
		this.#signals.close();
	}
}
