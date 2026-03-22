import { PRIORITY } from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import * as Zod from "@moq/lite/zod";
import { Effect, type Getter, Signal } from "@moq/signals";
import { type Location, PeersSchema, type Position } from "../sections";

export interface PeersProps {
	enabled?: boolean | Signal<boolean>;
}

export class Peers {
	enabled: Signal<boolean>;
	broadcast: Signal<Moq.Broadcast | undefined>;

	#peersTrack = new Signal<{ name: string } | undefined>(undefined);
	#positions = new Signal<Record<string, Position> | undefined>(undefined);

	signals = new Effect();

	constructor(
		broadcast: Signal<Moq.Broadcast | undefined>,
		locationSection: Getter<Location | undefined>,
		props?: PeersProps,
	) {
		this.broadcast = broadcast;
		this.enabled = Signal.from(props?.enabled ?? false);

		this.signals.run((effect) => {
			this.#peersTrack.set(effect.get(locationSection)?.peers);
		});

		this.signals.run(this.#run.bind(this));
	}

	#run(effect: Effect) {
		const values = effect.getAll([this.enabled, this.#peersTrack, this.broadcast]);
		if (!values) return;
		const [_, catalog, broadcast] = values;

		const track = broadcast.subscribe(catalog.name, PRIORITY.location);
		effect.cleanup(() => track.close());

		effect.spawn(this.#runTrack.bind(this, track));
	}

	async #runTrack(track: Moq.Track) {
		try {
			for (;;) {
				const frame = await Zod.read(track, PeersSchema);
				if (!frame) break;

				this.#positions.set(frame);
			}
		} finally {
			this.#positions.set(undefined);
			track.close();
		}
	}

	get positions(): Getter<Record<string, Position> | undefined> {
		return this.#positions;
	}

	close() {
		this.signals.close();
	}
}
