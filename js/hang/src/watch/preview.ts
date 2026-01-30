import type * as Moq from "@moq/lite";
import * as Zod from "@moq/lite/zod";
import { Effect, Signal } from "@moq/signals";
import * as Catalog from "../catalog";

export interface PreviewProps {
	enabled?: boolean | Signal<boolean>;
}

export class Preview {
	broadcast: Signal<Moq.Broadcast | undefined>;
	enabled: Signal<boolean>;
	preview = new Signal<Catalog.Preview | undefined>(undefined);
	#catalog = new Signal<Catalog.Track | undefined>(undefined);

	#signals = new Effect();

	constructor(
		broadcast: Signal<Moq.Broadcast | undefined>,
		catalog: Signal<Catalog.Root | undefined>,
		props?: PreviewProps,
	) {
		this.broadcast = broadcast;
		this.enabled = Signal.from(props?.enabled ?? false);

		this.#signals.effect((effect) => {
			this.#catalog.set(effect.get(catalog)?.preview);
		});

		this.#signals.effect((effect) => {
			const values = effect.getAll([this.enabled, this.broadcast, this.#catalog]);
			if (!values) return;
			const [_, broadcast, catalog] = values;

			// Subscribe to the preview.json track directly
			const track = broadcast.subscribe(catalog.name, catalog.priority);
			effect.cleanup(() => track.close());

			effect.spawn(async () => {
				try {
					const info = await Zod.read(track, Catalog.PreviewSchema);
					if (!info) return;

					this.preview.set(info);
				} catch (error) {
					console.warn("Failed to parse preview JSON:", error);
				}
			});

			effect.cleanup(() => this.preview.set(undefined));
		});
	}

	close() {
		this.#signals.close();
	}
}
