import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import * as Zod from "@moq/lite/zod";
import { Effect, type Getter, Signal } from "@moq/signals";

export interface DetectionProps {
	// Whether to subscribe to the detection track.
	// Defaults to false so consumers can opt in.
	enabled?: boolean | Signal<boolean>;
}

// Subscribes to the detection track advertised by the catalog and exposes
// the most recent set of bounding boxes.
export class Detection {
	broadcast: Getter<Moq.Broadcast | undefined>;
	enabled: Signal<boolean>;

	#catalog = new Signal<Catalog.Detection | undefined>(undefined);
	readonly catalog: Getter<Catalog.Detection | undefined> = this.#catalog;

	#latest = new Signal<Catalog.Detections | undefined>(undefined);
	readonly latest: Getter<Catalog.Detections | undefined> = this.#latest;

	signals = new Effect();

	constructor(
		broadcast: Getter<Moq.Broadcast | undefined>,
		catalog: Getter<Catalog.Root | undefined>,
		props?: DetectionProps,
	) {
		this.broadcast = broadcast;
		this.enabled = Signal.from(props?.enabled ?? false);

		this.signals.run((effect) => {
			this.#catalog.set(effect.get(catalog)?.detection);
		});

		this.signals.run(this.#run.bind(this));
	}

	#run(effect: Effect) {
		if (!effect.get(this.enabled)) return;

		const broadcast = effect.get(this.broadcast);
		if (!broadcast) return;

		const updates = effect.get(this.#catalog)?.track;
		if (!updates) return;

		const track = broadcast.subscribe(updates.name, Catalog.PRIORITY.detection);
		effect.cleanup(() => track.close());

		effect.cleanup(() => this.#latest.set(undefined));

		effect.spawn(this.#runTrack.bind(this, track));
	}

	async #runTrack(track: Moq.Track) {
		try {
			for (;;) {
				const detections = await Zod.read(track, Catalog.DetectionsSchema);
				if (!detections) break;

				this.#latest.set(detections);
			}
		} finally {
			this.#latest.set(undefined);
			track.close();
		}
	}

	close() {
		this.signals.close();
	}
}
