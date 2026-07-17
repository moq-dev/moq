import type * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";

/**
 * A function that checks if a video configuration can be played.
 *
 * `Decoder.supported` is the WebCodecs probe used by `<moq-watch>`.
 */
export type Supported = (config: Catalog.VideoConfig) => Promise<boolean>;

/** A video source error that prevents choosing a usable rendition. */
export type SourceError = "unsupported";

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

export type SourceInput = {
	broadcast: Getter<Broadcast | undefined>;
	target: Getter<Target | undefined>;

	// A function that checks if a video configuration can be played. Renditions that fail the
	// probe are filtered out. Nothing is selected until one is provided.
	supported: Getter<Supported | undefined>;
};

type SourceOutput = {
	catalog: Signal<Catalog.Video | undefined>;
	available: Signal<Record<string, Catalog.VideoConfig>>;

	// The current source error, or undefined while healthy or still probing.
	error: Signal<SourceError | undefined>;

	// The name of the active rendition.
	track: Signal<string | undefined>;
	config: Signal<Catalog.VideoConfig | undefined>;

	// The per-rendition jitter (ms) to add to the sync buffer. Wired into Sync by the parent.
	jitter: Signal<Moq.Time.Milli | undefined>;
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

		// No entries had resolution metadata — return all names unranked.
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

		// No entries had resolution metadata — return all names unranked.
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

		// No entries had bitrate metadata — return all names unranked.
		return entries.map(([name]) => name);
	};
}

/**
 * Pick the best rendition when no filters are active.
 * Prefers the largest resolution, falls back to highest bitrate,
 * then falls back to the first entry.
 */
function bestRendition(entries: [string, Catalog.VideoConfig][]): string {
	let best = entries[0];

	for (const entry of entries) {
		const [, config] = entry;
		const [, bestConfig] = best;

		const size = (config.codedWidth ?? 0) * (config.codedHeight ?? 0);
		const bestSize = (bestConfig.codedWidth ?? 0) * (bestConfig.codedHeight ?? 0);

		if (size !== bestSize) {
			if (size > bestSize) best = entry;
			continue;
		}

		if ((config.bitrate ?? 0) > (bestConfig.bitrate ?? 0)) {
			best = entry;
		}
	}

	return best[0];
}

/**
 * Source handles catalog extraction, support checking, and rendition selection
 * for video playback. The Decoder consumes whichever rendition it picks.
 */
export class Source {
	readonly in: Readonlys<SourceInput>;

	readonly #out: SourceOutput = {
		catalog: new Signal<Catalog.Video | undefined>(undefined),
		available: new Signal<Record<string, Catalog.VideoConfig>>({}),
		error: new Signal<SourceError | undefined>(undefined),
		track: new Signal<string | undefined>(undefined),
		config: new Signal<Catalog.VideoConfig | undefined>(undefined),
		jitter: new Signal<Moq.Time.Milli | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

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

		const renditions = effect.get(this.#out.catalog)?.renditions ?? {};
		this.#out.error.set(undefined);

		effect.spawn(async () => {
			const available: Record<string, Catalog.VideoConfig> = {};

			for (const [name, config] of Object.entries(renditions)) {
				let isSupported = false;
				try {
					isSupported = await supported(config);
				} catch (err) {
					console.warn(
						`[Source] video rendition ${name} (${config.codec}) support probe failed; treating as unsupported`,
						err,
					);
				}
				if (isSupported) available[name] = config;
			}

			const error =
				Object.keys(available).length === 0 && Object.keys(renditions).length > 0 ? "unsupported" : undefined;
			if (error === "unsupported") {
				console.warn("[Source] No supported video renditions found:", renditions);
			}

			this.#out.error.set(error);
			this.#out.available.set(available);
		});
	}

	#runSelected(effect: Effect): void {
		const available = effect.get(this.#out.available);
		if (Object.keys(available).length === 0) return;

		const target = effect.get(this.in.target);

		// Manual selection by name — skip all ABR logic.
		if (target?.name && target.name in available) {
			const config = available[target.name];
			effect.set(this.#out.track, target.name);
			effect.set(this.#out.config, config);
			effect.set(this.#out.jitter, config.jitter !== undefined ? Time.Milli(config.jitter) : undefined);
			return;
		}

		// Auto-select: use recv bandwidth if no explicit bitrate target.
		let effectiveTarget = target;
		if (!target?.bitrate) {
			const broadcast = effect.get(this.in.broadcast);
			const connection = broadcast ? effect.get(broadcast.in.connection) : undefined;
			const recvBw = connection?.recvBandwidth;
			if (recvBw) {
				const estimate = effect.get(recvBw);
				if (estimate != null) {
					// Apply a safety margin (80%) to avoid oscillation.
					const safeBitrate = Math.round(estimate * 0.8);
					effectiveTarget = { ...target, bitrate: safeBitrate };
				}
			}
		}

		const selected = this.#select(available, effectiveTarget);
		if (!selected) return;

		const config = available[selected];

		effect.set(this.#out.track, selected);
		effect.set(this.#out.config, config);

		// Use catalog jitter if available, otherwise estimate from framerate.
		const jitter = config.jitter ?? (config.framerate ? Math.ceil(1000 / config.framerate) : undefined);
		effect.set(this.#out.jitter, jitter !== undefined ? Time.Milli(jitter) : undefined);
	}

	/**
	 * Select the best rendition using a generic filter system.
	 *
	 * Each enabled filter returns matching renditions sorted by preference.
	 * The first rendition present in every filter's output is selected.
	 * If no rendition satisfies all filters, a warning is logged.
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

		// No filters — pick the best rendition by quality.
		if (filters.length === 0) {
			return bestRendition(entries);
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

		console.warn("conflicting rendition filters, no rendition satisfies all criteria");
		return undefined;
	}

	close(): void {
		this.#signals.close();
	}
}
