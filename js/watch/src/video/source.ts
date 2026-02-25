import type * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import type { Sync } from "../sync";

/**
 * A function that checks if a video configuration is supported by the backend.
 */
export type Supported = (config: Catalog.VideoConfig) => Promise<boolean>;

export type SourceProps = {
	broadcast?: Broadcast | Signal<Broadcast | undefined>;
	target?: Target | Signal<Target | undefined>;
	supported?: Supported;
};

export type Target = {
	// Optional manual override for the selected rendition name.
	name?: string;

	// The desired size of the video in pixels.
	pixels?: number;

	// TODO bitrate
};

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
	supported: Signal<Supported | undefined>;

	#signals = new Effect();

	constructor(sync: Sync, props?: SourceProps) {
		this.broadcast = Signal.from(props?.broadcast);
		this.target = Signal.from(props?.target);
		this.sync = sync;
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
		effect.set(this.sync.video, config.jitter as Moq.Time.Milli | undefined);
	}

	/**
	 * Select the best rendition based on target pixel count.
	 * Rounds up to the closest larger rendition, or falls back to the largest smaller one.
	 */
	#select(renditions: Record<string, Catalog.VideoConfig>, target?: Target): string | undefined {
		const entries = Object.entries(renditions);
		if (entries.length === 0) return undefined;
		if (entries.length === 1) return entries[0][0];

		// If we have no target, then choose the largest supported rendition.
		const pixels = target?.pixels ?? Number.MAX_SAFE_INTEGER / 2 - 1;

		// Round up to the closest rendition.
		// Also keep track of the 2nd closest, just in case there's nothing larger.

		let larger: string | undefined;
		let largerSize: number | undefined;

		let smaller: string | undefined;
		let smallerSize: number | undefined;

		for (const [name, config] of entries) {
			if (!config.codedHeight || !config.codedWidth) continue;

			const size = config.codedHeight * config.codedWidth;
			if (size > pixels && (!largerSize || size < largerSize)) {
				larger = name;
				largerSize = size;
			} else if (size < pixels && (!smallerSize || size > smallerSize)) {
				smaller = name;
				smallerSize = size;
			}
		}
		if (larger) return larger;
		if (smaller) return smaller;

		console.warn("no width/height information, choosing the first supported rendition");
		return entries[0][0];
	}

	close(): void {
		this.#signals.close();
	}
}
