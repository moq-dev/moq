import type * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";

// AudioWorklet always renders in 128-sample quanta.
const WORKLET_QUANTUM = 128;

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

export type SourceInput = {
	broadcast: Getter<Broadcast | undefined>;

	// The desired rendition/bitrate of the audio.
	target: Getter<Target | undefined>;

	// A function that checks if an audio configuration can be played. Renditions that fail the
	// probe are filtered out. Nothing is selected until one is provided.
	supported: Getter<Supported | undefined>;
};

type SourceOutput = {
	catalog: Signal<Catalog.Audio | undefined>;
	available: Signal<Record<string, Catalog.AudioConfig>>;
	track: Signal<string | undefined>;
	config: Signal<Catalog.AudioConfig | undefined>;

	// The per-rendition jitter (ms) to add to the sync buffer. Wired into Sync by the parent.
	jitter: Signal<Moq.Time.Milli | undefined>;
};

/**
 * Source handles catalog extraction, support checking, and rendition selection
 * for audio playback. The Decoder consumes whichever rendition it picks.
 */
export class Source {
	readonly in: Readonlys<SourceInput>;

	readonly #out: SourceOutput = {
		catalog: new Signal<Catalog.Audio | undefined>(undefined),
		available: new Signal<Record<string, Catalog.AudioConfig>>({}),
		track: new Signal<string | undefined>(undefined),
		config: new Signal<Catalog.AudioConfig | undefined>(undefined),
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

		const catalog = effect.get(broadcast.out.catalog)?.audio;
		if (!catalog) return;

		effect.set(this.#out.catalog, catalog);
	}

	#runSupported(effect: Effect): void {
		const renditions = effect.get(this.#out.catalog)?.renditions ?? {};
		const supported = effect.get(this.in.supported);
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

			this.#out.available.set(available);
		});
	}

	#runSelected(effect: Effect): void {
		const available = effect.get(this.#out.available);
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

		// Use catalog jitter if available, otherwise estimate from codec frame duration.
		// Add the worklet render quantum so the ring buffer has margin between frame arrivals.
		const codecJitter = selected.config.jitter ?? defaultAudioJitter(selected.config) ?? 0;
		const overhead = Math.ceil((WORKLET_QUANTUM / selected.config.sampleRate) * 1000);
		const jitter = codecJitter + overhead;
		effect.set(this.#out.jitter, Time.Milli(jitter));
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

	close(): void {
		this.#signals.close();
	}
}

// Estimate the minimum jitter (frame duration) based on the audio codec.
// TODO these are defaults; the actual frame duration depends on encoder config.
function defaultAudioJitter(config: Catalog.AudioConfig): number | undefined {
	if (config.codec.startsWith("opus")) {
		// Opus supports 2.5–60ms but 20ms is the real-time default.
		return 20;
	}

	if (config.codec.startsWith("mp4a")) {
		// 1024 samples for LC-AAC; HE-AAC/AAC-LD use different sizes.
		return Math.ceil((1024 / config.sampleRate) * 1000);
	}

	if (config.codec === "mp3") {
		// 1152 samples per frame for MPEG-1 Layer III; MPEG-2/2.5 use 576.
		const samples = config.sampleRate >= 32000 ? 1152 : 576;
		return Math.ceil((samples / config.sampleRate) * 1000);
	}

	return undefined;
}
