import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";

export type PreviewProps = {
	enabled?: boolean | Signal<boolean>;
	info?: Catalog.Preview | Signal<Catalog.Preview | undefined>;
};

export class Preview {
	static readonly TRACK = "preview.json";
	static readonly PRIORITY = Catalog.PRIORITY.preview;

	enabled: Signal<boolean>;
	info: Signal<Catalog.Preview | undefined>;

	catalog = new Signal<Catalog.Track | undefined>(undefined);

	signals = new Effect();

	constructor(props?: PreviewProps) {
		this.enabled = Signal.from(props?.enabled ?? false);
		this.info = Signal.from(props?.info);

		this.signals.run((effect) => {
			if (!effect.get(this.enabled)) return;
			effect.set(this.catalog, { name: Preview.TRACK });
		});
	}

	serve(track: Moq.Track, effect: Effect): void {
		const values = effect.getAll([this.enabled, this.info]);
		if (!values) return;
		const [_, info] = values;

		track.writeJson(info);
	}

	close() {
		this.signals.close();
	}
}
