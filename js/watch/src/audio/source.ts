import { Milli } from "@moq/clock";
import type * as Catalog from "@moq/hang/catalog";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { SupportProbe } from "../support-probe";
import { supported as decoderSupported } from "./supported";

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
	// probe are filtered out. Defaults to `Decoder.supported`, the WebCodecs probe.
	supported: Getter<Supported | undefined>;
};

/** Signals the source derives. */
export type SourceOutput = {
	catalog: Signal<Catalog.Audio | undefined>;
	available: Signal<Record<string, Catalog.AudioConfig>>;

	// Why no rendition can be chosen, or undefined while healthy or still probing. A
	// `NotSupportedError` means the browser can decode none of the catalog's renditions.
	error: Signal<Error | undefined>;

	track: Signal<string | undefined>;
	config: Signal<Catalog.AudioConfig | undefined>;

	/**
	 * The jitter the selected rendition advertises: a floor on the decoder's playout target.
	 *
	 * @internal Feeds the decoder.
	 */
	jitter: Signal<Milli | undefined>;
};

/**
 * Source handles catalog extraction, support checking, and rendition selection
 * for audio playback. The Decoder consumes whichever rendition it picks.
 */
export class Source {
	/** The wired inputs, read-only. */
	readonly in: Readonlys<SourceInput>;

	#probe = new SupportProbe<Catalog.AudioConfig>("audio");

	readonly #out: SourceOutput = {
		catalog: new Signal<Catalog.Audio | undefined>(undefined),
		available: new Signal<Record<string, Catalog.AudioConfig>>({}),
		error: this.#probe.error,
		track: new Signal<string | undefined>(undefined),
		config: new Signal<Catalog.AudioConfig | undefined>(undefined),
		jitter: new Signal<Milli | undefined>(undefined),
	};
	/** The derived outputs, read-only. */
	readonly out = readonlys(this.#out);
	/** Whether rendition support is still being resolved. @internal */
	readonly pending: Getter<boolean> = this.#probe.pending;

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
		const supported = effect.get(this.in.supported) ?? decoderSupported;
		// Read the catalog from the broadcast itself, so the probe result is tagged with the right one.
		const broadcast = effect.get(this.in.broadcast);
		const renditions = (broadcast && effect.get(broadcast.out.catalog)?.audio?.renditions) ?? {};
		this.#probe.run(effect, supported, renditions, (available) => this.#out.available.set(available));
	}

	#runSelected(effect: Effect): void {
		const target = effect.get(this.in.target);

		// A disabled rendition has no frames coming, so it is never selected, even by name.
		const available = Object.fromEntries(
			Object.entries(effect.get(this.#out.available)).filter(([, config]) => config.enabled !== false),
		);

		const track = target?.name && target.name in available ? target.name : this.#select(available);
		if (!track) return;

		const config = available[track];
		effect.set(this.#out.track, track);
		effect.set(this.#out.config, config);
		effect.set(this.#out.jitter, config.jitter !== undefined ? Milli(config.jitter) : undefined);
	}

	/**
	 * Select rendition based on the configured strategy.
	 */
	#select(renditions: Record<string, Catalog.AudioConfig>): string | undefined {
		const entries = Object.entries(renditions);
		for (const kind of ["legacy", "loc", "cmaf"]) {
			const found = entries.find(([, config]) => config.container.kind === kind);
			if (found) return found[0];
		}
		return undefined;
	}

	/** Stop and release everything this holds. */
	close(): void {
		this.#signals.close();
	}
}
