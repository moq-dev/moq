import { PRIORITY } from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import * as Zod from "@moq/lite/zod";
import { Effect, type Getter, Signal } from "@moq/signals";
import { type Location, type Position, PositionSchema } from "../sections";

export interface WindowProps {
	enabled?: boolean | Signal<boolean>;
}

export class Window {
	broadcast: Signal<Moq.Broadcast | undefined>;

	enabled: Signal<boolean>;

	#handle = new Signal<string | undefined>(undefined);
	readonly handle: Getter<string | undefined> = this.#handle;

	#location = new Signal<Location | undefined>(undefined);

	#position = new Signal<Position | undefined>(undefined);
	readonly position: Getter<Position | undefined> = this.#position;

	signals = new Effect();

	constructor(
		broadcast: Signal<Moq.Broadcast | undefined>,
		locationSection: Getter<Location | undefined>,
		props?: WindowProps,
	) {
		this.broadcast = broadcast;
		this.enabled = Signal.from(props?.enabled ?? false);

		this.signals.run((effect) => {
			this.#location.set(effect.get(locationSection));
		});

		this.signals.run((effect) => {
			if (!effect.get(this.enabled)) return;
			this.#position.set(effect.get(this.#location)?.initial);
		});

		this.signals.run((effect) => {
			this.#handle.set(effect.get(this.#location)?.handle);
		});

		this.signals.run((effect) => {
			const broadcast = effect.get(this.broadcast);
			if (!broadcast) return;

			const updates = effect.get(this.#location)?.track;
			if (!updates) return;

			const track = broadcast.subscribe(updates.name, PRIORITY.location);
			effect.cleanup(() => track.close());

			effect.spawn(this.#runTrack.bind(this, track));
		});
	}

	async #runTrack(track: Moq.Track) {
		try {
			for (;;) {
				const position = await Zod.read(track, PositionSchema);
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
