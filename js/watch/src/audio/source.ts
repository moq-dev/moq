import { Milli } from "@moq/clock";
import type * as Catalog from "@moq/hang/catalog";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";

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

	readonly #out: SourceOutput = {
		catalog: new Signal<Catalog.Audio | undefined>(undefined),
		available: new Signal<Record<string, Catalog.AudioConfig>>({}),
		track: new Signal<string | undefined>(undefined),
		config: new Signal<Catalog.AudioConfig | undefined>(undefined),
		jitter: new Signal<Milli | undefined>(undefined),
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
		// Read the catalog from the broadcast itself, so the probe result is tagged with the right one.
		const broadcast = effect.get(this.in.broadcast);
		const renditions = (broadcast && effect.get(broadcast.out.catalog)?.audio?.renditions) ?? {};
		const supported = effect.get(this.in.supported);
		if (!supported) return;

		effect.set(this.#pending, true, false);
		effect.spawn(async () => {
			try {
				const available: Record<string, Catalog.AudioConfig> = {};

				for (const [name, config] of Object.entries(renditions)) {
					// `supported` comes from the consumer, so we cannot assume it ever settles. A rerun
					// waits for the tasks it spawned, so an unraced probe would hold the next run shut.
					let isSupported = false;
					try {
						isSupported = !!(await effect.race(supported(config)));
					} catch (err) {
						if (effect.abort.aborted) return;
						console.warn(
							`[Source] audio rendition ${name} (${config.codec}) support probe failed; treating as unsupported`,
							err,
						);
					}

					// Torn down: stop probing and publish nothing, since the rerun redoes this.
					if (effect.abort.aborted) return;

					if (isSupported) available[name] = config;
				}

				if (Object.keys(available).length === 0 && Object.keys(renditions).length > 0) {
					console.warn("no supported audio renditions found:", renditions);
				}

				this.#probed.set(broadcast);
				this.#out.available.set(available);
			} finally {
				this.#pending.set(false);
			}
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
