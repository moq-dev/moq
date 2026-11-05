import { Milli } from "@moq/clock";
import type * as Catalog from "@moq/hang/catalog";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";
import { SupportProbe } from "../support-probe";

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

	// Why no rendition can be chosen, or undefined while healthy or still probing. A
	// `NotSupportedError` means the browser can decode none of the catalog's renditions.
	error: Signal<Error | undefined>;

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

	// The last selection, which a disable holds while no enabled rendition remains.
	#held?: { broadcast: Broadcast; track: string };

	// The broadcast `available` was probed for. It lags a broadcast switch until the new probe lands.
	// A signal, since the new broadcast's renditions can equal the old ones and leave `available` unchanged.
	#probed = new Signal<Broadcast | undefined>(undefined);

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
		const supported = effect.get(this.in.supported);
		// Read the catalog from the broadcast itself, so the probe result is tagged with the right one.
		const broadcast = effect.get(this.in.broadcast);
		const renditions = (broadcast && effect.get(broadcast.out.catalog)?.audio?.renditions) ?? {};
		this.#probe.run(effect, supported, renditions, (available) => {
			this.#probed.set(broadcast);
			this.#out.available.set(available);
		});
	}

	#runSelected(effect: Effect): void {
		const broadcast = effect.get(this.in.broadcast);
		const all = effect.get(this.#out.available);
		const probed = effect.get(this.#probed);
		const target = effect.get(this.in.target);

		// A disabled rendition has no frames coming, so it is never newly selected, even by name.
		const available = Object.fromEntries(Object.entries(all).filter(([, config]) => config.enabled !== false));

		const track =
			target?.name && target.name in available
				? target.name
				: (this.#select(available) ?? this.#hold(broadcast, all));
		// A pick from the previous broadcast's renditions is not this broadcast's selection to hold.
		this.#held = track && broadcast && probed === broadcast ? { broadcast, track } : undefined;
		if (!track) return;

		const config = all[track];
		effect.set(this.#out.track, track);
		effect.set(this.#out.config, config);
		effect.set(this.#out.jitter, config.jitter !== undefined ? Milli(config.jitter) : undefined);
	}

	// Every rendition is disabled: keep the selected one, and its subscription, so the re-enable
	// plays without a resubscribe. A removed rendition, or another broadcast, starts over.
	#hold(broadcast: Broadcast | undefined, all: Record<string, Catalog.AudioConfig>): string | undefined {
		if (Object.values(all).some((config) => config.enabled !== false)) return undefined;
		const held = this.#held;
		if (!held || held.broadcast !== broadcast || !(held.track in all)) return undefined;
		return held.track;
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
