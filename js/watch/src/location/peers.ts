import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import * as Zod from "@moq/lite/zod";
import { Effect, type Getter, Signal } from "@moq/signals";

export interface PeersProps {
	enabled?: boolean | Signal<boolean>;
}

export class Peers {
	enabled: Signal<boolean>;
	broadcast: Signal<Moq.Broadcast | undefined>;

	#catalog = new Signal<Catalog.Track | undefined>(undefined);
	#positions = new Signal<Record<string, Catalog.Position> | undefined>(undefined);

	signals = new Effect();

	constructor(
		broadcast: Signal<Moq.Broadcast | undefined>,
		catalog: Signal<Catalog.Root | undefined>,
		props?: PeersProps,
	) {
		this.broadcast = broadcast;
		this.enabled = Signal.from(props?.enabled ?? false);

		this.signals.run((effect) => {
			this.#catalog.set(effect.get(catalog)?.location?.peers);
		});

		this.signals.run(this.#run.bind(this));
	}

	#run(effect: Effect) {
		const values = effect.getAll([this.enabled, this.#catalog, this.broadcast]);
		if (!values) return;
		const [_, catalog, broadcast] = values;

		const track = broadcast.subscribe(catalog.name, Catalog.PRIORITY.location);
		effect.cleanup(() => track.close());

		effect.spawn(this.#runTrack.bind(this, track));
	}

	async #runTrack(track: Moq.Track) {
		try {
			for (;;) {
				const frame = await Zod.read(track, Catalog.PeersSchema);
				if (!frame) break;

				this.#positions.set(frame);
			}
		} finally {
			this.#positions.set(undefined);
			track.close();
		}
	}

	get positions(): Getter<Record<string, Catalog.Position> | undefined> {
		return this.#positions;
	}

	close() {
		this.signals.close();
	}
}
