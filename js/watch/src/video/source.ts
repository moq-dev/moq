import * as Catalog from "@moq/hang/catalog";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { SupportProbe } from "../support-probe";

/**
 * A function that checks if a video configuration can be played.
 *
 * `Decoder.supported` is the WebCodecs probe used by `<moq-watch>`.
 */
export type Supported = (config: Catalog.VideoConfig) => Promise<boolean>;

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

	// Maximum desired bitrate in bits per second, a cap rather than a hint: selection picks the
	// best rendition within it, and `0` picks the lowest rendition with a known bitrate.
	// Omit it to select from the `rate` input instead.
	bitrate?: number;
};

/** Signals the source reads. */
export type SourceInput = {
	broadcast: Getter<Broadcast | undefined>;
	target: Getter<Target | undefined>;

	// A function that checks if a video configuration can be played. Renditions that fail the
	// probe are filtered out. Nothing is selected until one is provided.
	supported: Getter<Supported | undefined>;

	// The receive rate in bits per second this video may spend, used to auto-select a rendition
	// when the target has no explicit bitrate. A `Player` wires its viewer grant less its audio.
	// While `undefined`, selection keeps the last rate it had, or ranks alone without one.
	rate: Getter<number | undefined>;
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

	/**
	 * The receive rate, in bits per second, at which selection settles on the best rendition
	 * the target allows, or undefined when no candidate has a catalog bitrate. A player claims it
	 * as its viewer ceiling, so a grant that reaches it makes that rendition selectable.
	 *
	 * @internal Feeds the player's viewer claim.
	 */
	ceiling: Signal<number | undefined>;
};

// Automatic selection spends at most this share of the receive rate, to avoid oscillation.
const MARGIN = 0.8;

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

/** Return the renditions that may be selected: a disabled one has no frames coming. */
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

/** Whether the receive rate steers selection: not with nothing to select, or a pinned rendition or bitrate. */
function steers(available: Record<string, Catalog.VideoConfig>, target: Target | undefined): boolean {
	if (Object.keys(available).length === 0) return false;
	return !(target?.name && target.name in available) && target?.bitrate === undefined;
}

/**
 * Source handles catalog extraction, support checking, and rendition selection
 * for video playback. The Decoder consumes whichever rendition it picks.
 */
export class Source {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<SourceInput>;

	#probe = new SupportProbe<Catalog.VideoConfig>("video");

	readonly #out: SourceOutput = {
		catalog: new Signal<Catalog.Video | undefined>(undefined),
		available: new Signal<Record<string, Catalog.VideoConfig>>({}),
		error: this.#probe.error,
		track: new Signal<string | undefined>(undefined),
		config: new Signal<Catalog.VideoConfig | undefined>(undefined),
		ceiling: new Signal<number | undefined>(undefined),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);
	/** Whether rendition support is still being resolved. @internal */
	readonly pending: Getter<boolean> = this.#probe.pending;

	// The last defined `rate`, held while it is unknown, such as across a reconnect.
	#rate: number | undefined;

	#signals = new Effect();

	constructor(props?: Inputs<SourceInput>) {
		this.in = {
			broadcast: getter(props?.broadcast),
			target: getter(props?.target),
			supported: getter(props?.supported),
			rate: getter(props?.rate),
		};

		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runSupported.bind(this));
		this.#signals.run(this.#runSelected.bind(this));
		this.#signals.run(this.#runCeiling.bind(this));
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
		// Read the catalog from the broadcast itself, so the probe result is tagged with the right one.
		const broadcast = effect.get(this.in.broadcast);
		const renditions = (broadcast && effect.get(broadcast.out.catalog)?.video?.renditions) ?? {};
		this.#probe.run(effect, supported, renditions, (available) => this.#out.available.set(available));
	}

	#runSelected(effect: Effect): void {
		const available = enabledRenditions(effect.get(this.#out.available));
		const target = effect.get(this.in.target);
		// Read the rate only when it steers selection, so a pinned choice doesn't rerun with it.
		if (steers(available, target)) this.#rate = effect.get(this.in.rate) ?? this.#rate;
		const selected = this.#choose(available, target, this.#rate);
		if (!selected) return;

		effect.set(this.#out.track, selected);
		effect.set(this.#out.config, available[selected]);
	}

	#runCeiling(effect: Effect): void {
		const available = enabledRenditions(effect.get(this.#out.available));
		const target = effect.get(this.in.target);
		// An unlimited rate picks the best rendition bandwidth could ever buy.
		const best = this.#choose(available, target, Number.POSITIVE_INFINITY);
		const bitrate = best === undefined ? undefined : available[best].bitrate;
		if (bitrate === undefined) return;
		// Only the rate's discounted share counts, so it must reach the bitrate over the margin.
		const ceiling = steers(available, target) ? Math.ceil(bitrate / MARGIN) : bitrate;
		effect.set(this.#out.ceiling, ceiling, undefined);
	}

	// Pick among the enabled renditions, given the receive rate if known.
	#choose(
		available: Record<string, Catalog.VideoConfig>,
		target: Target | undefined,
		rate: number | undefined,
	): string | undefined {
		if (target?.name && target.name in available) return target.name;
		if (Object.keys(available).length === 0) return undefined;

		if (rate != null && steers(available, target)) {
			target = { ...target, bitrate: Math.round(rate * MARGIN) };
		}

		return this.#select(preferredRenditions(available), target);
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
