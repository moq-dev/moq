import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import * as Zod from "@moq/lite/zod";
import { Effect, type Getter, Signal } from "@moq/signals";

export interface WindowProps {
	enabled?: boolean | Signal<boolean>;
}

export class Window {
	broadcast: Signal<Moq.Broadcast | undefined>;

	enabled: Signal<boolean>;

	#handle = new Signal<string | undefined>(undefined);
	readonly handle: Getter<string | undefined> = this.#handle;

	#catalog = new Signal<Catalog.Location | undefined>(undefined);

	#position = new Signal<Catalog.Position | undefined>(undefined);
	readonly position: Getter<Catalog.Position | undefined> = this.#position;

	signals = new Effect();

	constructor(
		broadcast: Signal<Moq.Broadcast | undefined>,
		catalog: Signal<Catalog.Root | undefined>,
		props?: WindowProps,
	) {
		this.broadcast = broadcast;
		this.enabled = Signal.from(props?.enabled ?? false);

		this.signals.run((effect) => {
			this.#catalog.set(effect.get(catalog)?.location);
		});

		this.signals.run((effect) => {
			if (!effect.get(this.enabled)) return;
			this.#position.set(effect.get(this.#catalog)?.initial);
		});

		this.signals.run((effect) => {
			this.#handle.set(effect.get(this.#catalog)?.handle);
		});

		this.signals.run((effect) => {
			const broadcast = effect.get(this.broadcast);
			if (!broadcast) return;

			const updates = effect.get(this.#catalog)?.track;
			if (!updates) return;

			const track = broadcast.subscribe(updates.name, Catalog.PRIORITY.location);
			effect.cleanup(() => track.close());

			effect.spawn(this.#runTrack.bind(this, track));
		});
	}

	async #runTrack(track: Moq.Track) {
		try {
			for (;;) {
				const position = await Zod.read(track, Catalog.PositionSchema);
				if (!position) break;

				this.#position.set(position);
			}
		} finally {
			this.#position.set(undefined);
			track.close();
		}
	}

	close() {
		this.signals.close();
	}
}
