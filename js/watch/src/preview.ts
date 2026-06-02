import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import * as Zod from "@moq/net/zod";
import { Effect, type Getter, getter, type InputProps, type Readonlys, readonlys, Signal } from "@moq/signals";

type PreviewInput = {
	enabled: Getter<boolean>;
	broadcast: Getter<Moq.Broadcast | undefined>;
	catalog: Getter<Catalog.Root | undefined>;
};

type PreviewOutput = {
	preview: Signal<Catalog.Preview | undefined>;
};

export type PreviewProps = InputProps<PreviewInput>;

export class Preview {
	readonly input: Readonlys<PreviewInput>;

	readonly #output: PreviewOutput = {
		preview: new Signal<Catalog.Preview | undefined>(undefined),
	};
	readonly output = readonlys(this.#output);

	#catalog = new Signal<Catalog.Track | undefined>(undefined);

	#signals = new Effect();

	constructor(props?: PreviewProps) {
		this.input = {
			enabled: getter(props?.enabled ?? false),
			broadcast: getter(props?.broadcast),
			catalog: getter(props?.catalog),
		};

		this.#signals.run((effect) => {
			this.#catalog.set(effect.get(this.input.catalog)?.preview);
		});

		this.#signals.run((effect) => {
			const values = effect.getAll([this.input.enabled, this.input.broadcast, this.#catalog]);
			if (!values) return;
			const [_, broadcast, catalog] = values;

			// Subscribe to the preview.json track directly
			const track = broadcast.subscribe(catalog.name, Catalog.PRIORITY.preview);
			effect.cleanup(() => track.close());

			effect.spawn(async () => {
				try {
					const info = await Zod.read(track, Catalog.PreviewSchema);
					if (!info) return;

					this.#output.preview.set(info);
				} catch (error) {
					console.warn("Failed to parse preview JSON:", error);
				}
			});

			effect.cleanup(() => this.#output.preview.set(undefined));
		});
	}

	close() {
		this.#signals.close();
	}
}
