import type * as Catalog from "@moq/hang/catalog";
import { Time } from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import type { Broadcast } from "../broadcast";

// The cue formats the renderer knows how to display. Renditions in any other format (e.g. "ttml")
// are still listed in `available` for a picker, but selecting one warns and renders nothing.
const RENDERABLE = new Set(["vtt", "utf8"]);

/** Whether the renderer can display a text rendition's cue format. */
export function renderable(config: Catalog.TextConfig): boolean {
	return RENDERABLE.has(config.format);
}

export type SourceInput = {
	broadcast: Getter<Broadcast | undefined>;

	// The selected caption track name, or `undefined` for off (the default; captions are opt-in).
	target: Getter<string | undefined>;
};

type SourceOutput = {
	// The catalog text section, or undefined when the broadcast publishes no captions.
	catalog: Signal<Catalog.Text | undefined>;

	// Every text rendition, keyed by track name, for a picker to list.
	available: Signal<Record<string, Catalog.TextConfig>>;

	// The selected track name and its config, or undefined when off / not found.
	track: Signal<string | undefined>;
	config: Signal<Catalog.TextConfig | undefined>;

	// The selected rendition's jitter (ms) to fold into the sync buffer. Wired into Sync by the
	// parent: a publisher that flushes cues less often than it flushes frames needs the extra
	// buffer, or short cues land after their display window and are never shown.
	jitter: Signal<Time.Milli | undefined>;
};

/**
 * Selects a caption rendition from the catalog `text` section.
 *
 * Unlike audio/video there is no support probe and no automatic selection: captions are opt-in, so
 * nothing is chosen until `target` names a track. The {@link Renderer} consumes whatever it picks.
 */
export class Source {
	readonly in: Readonlys<SourceInput>;

	readonly #out: SourceOutput = {
		catalog: new Signal<Catalog.Text | undefined>(undefined),
		available: new Signal<Record<string, Catalog.TextConfig>>({}),
		track: new Signal<string | undefined>(undefined),
		config: new Signal<Catalog.TextConfig | undefined>(undefined),
		jitter: new Signal<Time.Milli | undefined>(undefined),
	};
	readonly out = readonlys(this.#out);

	#signals = new Effect();

	constructor(props?: Inputs<SourceInput>) {
		this.in = {
			broadcast: getter(props?.broadcast),
			target: getter(props?.target),
		};

		this.#signals.run(this.#runCatalog.bind(this));
		this.#signals.run(this.#runSelected.bind(this));
	}

	#runCatalog(effect: Effect): void {
		const broadcast = effect.get(this.in.broadcast);
		const catalog = broadcast ? effect.get(broadcast.out.catalog)?.text : undefined;

		effect.set(this.#out.catalog, catalog);
		this.#out.available.set(catalog?.renditions ?? {});
	}

	#runSelected(effect: Effect): void {
		const available = effect.get(this.#out.available);
		const target = effect.get(this.in.target);

		const config = target ? available[target] : undefined;
		if (!target || !config) {
			effect.set(this.#out.track, undefined);
			effect.set(this.#out.config, undefined);
			effect.set(this.#out.jitter, undefined);
			return;
		}

		if (!renderable(config)) {
			console.warn(`captions: unsupported format ${JSON.stringify(config.format)} for track ${target}`);
		}

		effect.set(this.#out.track, target);
		effect.set(this.#out.config, config);
		effect.set(this.#out.jitter, config.jitter !== undefined ? Time.Milli(config.jitter) : undefined);
	}

	close(): void {
		this.#signals.close();
	}
}
