import type * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";

/**
 * A function that checks if a video configuration is supported by the backend.
 */
export type Supported = (config: Catalog.VideoConfig) => Promise<boolean>;

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

type SourceInput = {
	broadcast: Getter<Broadcast | undefined>;
	target: Getter<Target | undefined>;

	// A function that checks if a video configuration is supported by the backend.
	// Provided by whichever backend (WebCodecs or MSE) is active.
	supported: Getter<Supported | undefined>;
};

type SourceOutput = {
	catalog: Signal<Catalog.Video | undefined>;
	available: Signal<Record<string, Catalog.VideoConfig>>;

	// True once we've probed the catalog's renditions and this browser/hardware can decode none of
	// them. Distinct from "still probing" (both leave `available` empty), so the UI can show an
	// "unsupported codec" notice instead of an indefinite spinner. Expected on some hardware, since
	// codec support varies, so it is not necessarily a bug.
	unsupported: Signal<boolean>;

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
 * for video playback. It is used by both MSE and Decoder backends.
 */
export class Source {
	readonly input: Readonlys<SourceInput>;

	readonly #output: SourceOutput = {
		catalog: new Signal<Catalog.Video | undefined>(undefined),
		available: new Signal<Record<string, Catalog.VideoConfig>>({}),
		unsupported: new Signal<boolean>(false),
		track: new Signal<string | undefined>(undefined),
		config: new Signal<Catalog.VideoConfig | undefined>(undefined),
		jitter: new Signal<Moq.Time.Milli | undefined>(undefined),
	};
	readonly output = readonlys(this.#output);

	#signals = new Effect();

	constructor(props?: Inputs<SourceInput>) {
		this.input = {
			broadcast: getter(props?.broadcast),
			target: getter(props?.target),
			supported: getter(props?.supported),
		};

		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runSupported.bind(this));
		this.#signals.run(this.#runSelected.bind(this));
	}

	#runCatalog(effect: Effect): void {
		const broadcast = effect.get(this.input.broadcast);
		if (!broadcast) return;

		const catalog = effect.get(broadcast.output.catalog)?.video;
		if (!catalog) return;

		effect.set(this.#output.catalog, catalog);
	}

	#runSupported(effect: Effect): void {
		const supported = effect.get(this.input.supported);
		if (!supported) return;

		const renditions = effect.get(this.#output.catalog)?.renditions ?? {};

		// Drop renditions whose codec/container no longer match BEFORE the async probe: #config only
		// updates after `supported()` resolves, so a publisher codec switch in that window would decode
		// the new stream under the old config. A description-only change is NOT pruned: description is a
		// decoder-reload field handled by the normal reconfigure, and pruning would yank HD down to SD on
		// a benign description republish. Still-matching renditions keep their identical value (a no-op set).
		const stillValid: Record<string, Catalog.VideoConfig> = {};
		for (const [name, cfg] of Object.entries(this.#output.available.peek())) {
			const next = renditions[name];
			if (next && next.codec === cfg.codec && next.container?.kind === cfg.container?.kind) {
				stillValid[name] = cfg;
			}
		}
		this.#output.available.set(stillValid);

		effect.spawn(async () => {
			const available: Record<string, Catalog.VideoConfig> = {};

			for (const [name, config] of Object.entries(renditions)) {
				// supported() can THROW (malformed description hex, or a codec string that makes
				// isConfigSupported reject). A throw must not abort the loop: that would drop every rendition
				// (including decodable ones) and leave #unsupported/#available unset, so the unsupported
				// indicator never shows and the viewer spins forever. Treat a throw as unsupported.
				let isSupported = false;
				try {
					isSupported = await supported(config);
				} catch (err) {
					console.warn(
						`[Source] video rendition ${name} (${config.codec}) probe threw; treating as unsupported`,
						err,
					);
				}
				if (isSupported) available[name] = config;
			}

			const unsupported = Object.keys(available).length === 0 && Object.keys(renditions).length > 0;
			if (unsupported) {
				console.warn("[Source] No supported video renditions found:", renditions);
			}

			this.#output.unsupported.set(unsupported);
			this.#output.available.set(available);
		});
	}

	#runSelected(effect: Effect): void {
		const available = effect.get(this.#output.available);
		if (Object.keys(available).length === 0) return;

		const target = effect.get(this.input.target);

		// Manual selection by name — skip all ABR logic.
		if (target?.name && target.name in available) {
			const config = available[target.name];
			effect.set(this.#output.track, target.name);
			effect.set(this.#output.config, config);
			effect.set(this.#output.jitter, config.jitter !== undefined ? Time.Milli(config.jitter) : undefined);
			return;
		}

		// Auto-select: use recv bandwidth if no explicit bitrate target.
		let effectiveTarget = target;
		if (!target?.bitrate) {
			const broadcast = effect.get(this.input.broadcast);
			const connection = broadcast ? effect.get(broadcast.input.connection) : undefined;
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

		effect.set(this.#output.track, selected);
		effect.set(this.#output.config, config);

		// Use catalog jitter if available, otherwise estimate from framerate.
		const jitter = config.jitter ?? (config.framerate ? Math.ceil(1000 / config.framerate) : undefined);
		effect.set(this.#output.jitter, jitter !== undefined ? Time.Milli(jitter) : undefined);
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
