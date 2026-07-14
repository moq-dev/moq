import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Effect, type Getter, Signal } from "@moq/signals";
import { Encoder, type EncoderConfig, type EncoderProps, type Stats } from "./encoder";
import { TrackProcessor } from "./polyfill";
import type { Source } from "./types";

export * from "./encoder";
export * from "./types";

export type Props = {
	source?: Source | Signal<Source | undefined>;
	hd?: EncoderProps;
	sd?: EncoderProps;
	flip?: boolean | Signal<boolean>;
	connection?: Getter<Moq.Connection.Established | undefined>;
};

export class Root {
	static readonly TRACK_HD = "video/hd";
	static readonly TRACK_SD = "video/sd";
	static readonly PRIORITY = Catalog.PRIORITY.video;

	// Default the sd rendition to ~3/16 of the source pixel count (e.g. ~480p from a 1080p source).
	// Scaling relative to the source keeps simulcast generic: we don't assume the hd resolution or
	// bake in a fixed baseline. Without a cap the sd encoder would run at the full source resolution,
	// duplicating hd. Only applied when no sd config is provided; passing any config (even an empty
	// object or a Signal) takes full ownership, so `config: {}` removes the cap entirely.
	static readonly SD_DEFAULT_CONFIG: EncoderConfig = { maxScale: 0.1875 };

	source: Signal<Source | undefined>;
	hd: Encoder;
	sd: Encoder;

	#stats = new Signal<Stats & { hd?: Stats; sd?: Stats }>(aggregate([]));
	// Combined encoder stats, since simulcast splits video across two encoders (hd + sd). The top-level
	// totals sum every enabled rendition; hd/sd break them out. A rendition is present only while it's
	// enabled, so a consumer reads one getter (and its totals) instead of knowing about the split.
	readonly stats: Getter<Stats & { hd?: Stats; sd?: Stats }> = this.#stats;

	frame = new Signal<VideoFrame | undefined>(undefined);

	catalog = new Signal<Catalog.Video | undefined>(undefined);
	display = new Signal<{ width: number; height: number } | undefined>(undefined);
	flip = new Signal<boolean>(false);

	signals = new Effect();

	constructor(props?: Props) {
		this.source = Signal.from(props?.source);

		const connection = props?.connection ?? new Signal<Moq.Connection.Established | undefined>(undefined);
		this.hd = new Encoder(this.frame, this.source, connection, props?.hd);
		this.sd = new Encoder(this.frame, this.source, connection, {
			...props?.sd,
			config: props?.sd?.config ?? Root.SD_DEFAULT_CONFIG,
		});

		this.flip = Signal.from(props?.flip ?? false);

		this.signals.run(this.#runCatalog.bind(this));
		this.signals.run(this.#runFrame.bind(this));
		this.signals.run(this.#runStats.bind(this));
	}

	#runStats(effect: Effect) {
		const hd = effect.get(this.hd.enabled) ? effect.get(this.hd.stats) : undefined;
		const sd = effect.get(this.sd.enabled) ? effect.get(this.sd.stats) : undefined;
		const total = aggregate([hd, sd].filter((s): s is Stats => s !== undefined));
		this.#stats.set({ ...total, hd, sd });
	}

	#runFrame(effect: Effect) {
		const source = effect.get(this.source);
		if (!source) return;

		// NOTE: We modify the stock MediaStreamTrackProcessor so timestamps use our wall clock time.
		// This is so even when the source is changed or encoder reloaded, the timestamps will be consistent.
		const reader = TrackProcessor(source).getReader();
		effect.cleanup(() => reader.cancel());

		effect.spawn(async () => {
			for (;;) {
				const next = await Promise.race([reader.read(), effect.cancel]);
				if (!next?.value) break;

				this.frame.update((prev) => {
					prev?.close();
					return next.value;
				});

				this.display.set({ width: next.value.codedWidth, height: next.value.codedHeight });
			}
		});

		effect.cleanup(() => {
			this.frame.update((prev) => {
				prev?.close();
				return undefined;
			});
			this.display.set(undefined);
		});
	}

	#runCatalog(effect: Effect) {
		const source = effect.get(this.source);
		if (!source) return;

		const display = effect.get(this.display);
		if (!display) return;

		const hdConfig = effect.get(this.hd.catalog);
		const sdConfig = effect.get(this.sd.catalog);

		const renditions: Record<string, Catalog.VideoConfig> = {};
		if (hdConfig) renditions[Root.TRACK_HD] = hdConfig;
		if (sdConfig) renditions[Root.TRACK_SD] = sdConfig;

		const catalog: Catalog.Video = {
			renditions,
			display: {
				width: Catalog.u53(display.width),
				height: Catalog.u53(display.height),
			},
			flip: effect.get(this.flip) ?? undefined,
		};

		effect.set(this.catalog, catalog);
	}

	close() {
		this.signals.close();
		this.hd.close();
		this.sd.close();

		this.frame.update((prev) => {
			prev?.close();
			return undefined;
		});
	}
}

// Sum per-encoder Stats into a combined total (across simulcast renditions).
function aggregate(stats: Iterable<Stats>): Stats {
	const total: Stats = { frames: 0, bytes: 0, keyframes: 0 };
	for (const s of stats) {
		total.frames += s.frames;
		total.bytes += s.bytes;
		total.keyframes += s.keyframes;
	}
	return total;
}
