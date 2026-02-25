import type * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import type { Sync } from "../sync";

export type Target = {
	// Optional manual override for the selected rendition name.
	name?: string;
};

/**
 * A function that checks if an audio configuration is supported by the backend.
 */
export type Supported = (config: Catalog.AudioConfig) => Promise<boolean>;

export type SourceProps = {
	broadcast?: Broadcast | Signal<Broadcast | undefined>;

	// The desired rendition/bitrate of the audio.
	target?: Target | Signal<Target | undefined>;

	// A function that checks if an audio configuration is supported by the backend.
	supported?: Supported;
};

/**
 * Source handles catalog extraction, support checking, and rendition selection
 * for audio playback. It is used by both MSE and Decoder backends.
 */
export class Source {
	broadcast: Signal<Broadcast | undefined>;
	target: Signal<Target | undefined>;

	#catalog = new Signal<Catalog.Audio | undefined>(undefined);
	readonly catalog: Getter<Catalog.Audio | undefined> = this.#catalog;

	#available = new Signal<Record<string, Catalog.AudioConfig>>({});
	readonly available: Getter<Record<string, Catalog.AudioConfig>> = this.#available;

	#track = new Signal<string | undefined>(undefined);
	readonly track: Signal<string | undefined> = this.#track;

	#config = new Signal<Catalog.AudioConfig | undefined>(undefined);
	readonly config: Getter<Catalog.AudioConfig | undefined> = this.#config;

	supported: Signal<Supported | undefined>;

	// Used to target a latency and synchronize playback of video with audio.
	sync: Sync;

	#signals = new Effect();

	constructor(sync: Sync, props?: SourceProps) {
		this.sync = sync;

		this.broadcast = Signal.from(props?.broadcast);
		this.target = Signal.from(props?.target);
		this.supported = Signal.from(props?.supported);

		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runSupported.bind(this));
		this.#signals.run(this.#runSelected.bind(this));
	}

	#runCatalog(effect: Effect): void {
		const broadcast = effect.get(this.broadcast);
		if (!broadcast) return;

		const catalog = effect.get(broadcast.catalog)?.audio;
		if (!catalog) return;

		effect.set(this.#catalog, catalog);
	}

	#runSupported(effect: Effect): void {
		const renditions = effect.get(this.#catalog)?.renditions ?? {};
		const supported = effect.get(this.supported);
		if (!supported) return;

		effect.spawn(async () => {
			const available: Record<string, Catalog.AudioConfig> = {};

			for (const [name, config] of Object.entries(renditions)) {
				const isSupported = await supported(config);
				if (isSupported) available[name] = config;
			}

			if (Object.keys(available).length === 0 && Object.keys(renditions).length > 0) {
				console.warn("no supported audio renditions found:", renditions);
			}

			this.#available.set(available);
		});
	}

	#runSelected(effect: Effect): void {
		const available = effect.get(this.#available);
		if (Object.keys(available).length === 0) return;

		const target = effect.get(this.target);

		let selected: { track: string; config: Catalog.AudioConfig } | undefined;

		// Manual selection by name
		if (target?.name && target.name in available) {
			selected = { track: target.name, config: available[target.name] };
		} else {
			// Automatic selection
			selected = this.#select(available);
			if (!selected) return;
		}

		effect.set(this.#track, selected.track);
		effect.set(this.#config, selected.config);
		effect.set(this.sync.audio, selected.config.jitter as Moq.Time.Milli | undefined);
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
			if (config.container.kind === "cmaf") {
				return { track, config };
			}
		}

		return undefined;
	}

	close(): void {
		this.#signals.close();
	}
}
