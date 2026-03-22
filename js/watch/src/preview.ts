import { PRIORITY } from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import * as Zod from "@moq/lite/zod";
import { Effect, type Getter, Signal } from "@moq/signals";
import { type Preview as PreviewInfo, PreviewSchema, type PreviewTrack } from "./sections";

export interface PreviewProps {
	enabled?: boolean | Signal<boolean>;
}

export class Preview {
	broadcast: Signal<Moq.Broadcast | undefined>;
	enabled: Signal<boolean>;
	preview = new Signal<PreviewInfo | undefined>(undefined);

	#signals = new Effect();

	constructor(
		broadcast: Signal<Moq.Broadcast | undefined>,
		previewSection: Getter<PreviewTrack | undefined>,
		props?: PreviewProps,
	) {
		this.broadcast = broadcast;
		this.enabled = Signal.from(props?.enabled ?? false);

		this.#signals.run((effect) => {
			const values = effect.getAll([this.enabled, this.broadcast]);
			if (!values) return;
			const [_, broadcast] = values;

			const catalog = effect.get(previewSection);
			if (!catalog) return;

			// Subscribe to the preview.json track directly
			const track = broadcast.subscribe(catalog.name, PRIORITY.preview);
			effect.cleanup(() => track.close());

			effect.spawn(async () => {
				try {
					const info = await Zod.read(track, PreviewSchema);
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
