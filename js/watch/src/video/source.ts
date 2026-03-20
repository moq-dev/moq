import type * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { Sync, type SyncTrack } from "../sync";

/**
 * A function that checks if a video configuration is supported by the backend.
 */
export type Supported = (config: Catalog.VideoConfig) => Promise<boolean>;

export type SourceProps = {
	broadcast?: Broadcast | Signal<Broadcast | undefined>;
	target?: Target | Signal<Target | undefined>;
	supported?: Supported;
	// Shared Sync instance for synchronizing playback across tracks. Defaults to a standalone Sync.
	sync?: Sync;
};

export type Target = {
	// Optional manual override for the selected rendition name.
	name?: string;

	// The desired size of the video in pixels.
	pixels?: number;

	// Maximum desired bitrate in bits per second.
	bitrate?: number;
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
 * for video playback. It is used by both MSE and Decoder backends.
 */
export class Source {
	broadcast: Signal<Broadcast | undefined>;
	target: Signal<Target | undefined>;

	#catalog = new Signal<Catalog.Video | undefined>(undefined);
	readonly catalog: Getter<Catalog.Video | undefined> = this.#catalog;

	#available = new Signal<Record<string, Catalog.VideoConfig>>({});
	readonly available: Getter<Record<string, Catalog.VideoConfig>> = this.#available;

	// The name of the active rendition.
	#track = new Signal<string | undefined>(undefined);
	readonly track: Getter<string | undefined> = this.#track;

	#config = new Signal<Catalog.VideoConfig | undefined>(undefined);
	readonly config: Getter<Catalog.VideoConfig | undefined> = this.#config;

	sync: Sync;
	#syncTrack: SyncTrack;
	supported: Signal<Supported | undefined>;

	#signals = new Effect();

	constructor(props: SourceProps) {
		this.broadcast = Signal.from(props?.broadcast);
		this.target = Signal.from(props?.target);
		this.sync = props.sync ?? new Sync();
		this.#syncTrack = this.sync.track();
		this.supported = Signal.from(props?.supported);

		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runSupported.bind(this));
		this.#signals.run(this.#runSelected.bind(this));
	}

	#runCatalog(effect: Effect): void {
		const broadcast = effect.get(this.broadcast);
		if (!broadcast) return;

		const catalog = effect.get(broadcast.catalog)?.video;
		if (!catalog) return;

		effect.set(this.#catalog, catalog);
	}

	#runSupported(effect: Effect): void {
		const supported = effect.get(this.supported);
		if (!supported) return;

		const renditions = effect.get(this.#catalog)?.renditions ?? {};

		effect.spawn(async () => {
			const available: Record<string, Catalog.VideoConfig> = {};

			for (const [name, config] of Object.entries(renditions)) {
				const isSupported = await supported(config);
				if (isSupported) available[name] = config;
			}

			if (Object.keys(available).length === 0 && Object.keys(renditions).length > 0) {
				console.warn("[Source] No supported video renditions found:", renditions);
			}

			this.#available.set(available);
		});
	}

	#runSelected(effect: Effect): void {
		const available = effect.get(this.#available);
		if (Object.keys(available).length === 0) return;

		const target = effect.get(this.target);

		// Manual selection by name
		const manual = target?.name;
		const selected = manual && manual in available ? manual : this.#select(available, target);
		if (!selected) return;

		const config = available[selected];

		effect.set(this.#track, selected);
		effect.set(this.#config, config);
		this.#syncTrack.set(config.jitter as Moq.Time.Milli | undefined);
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
		this.#syncTrack.close();
		this.#signals.close();
	}
}
