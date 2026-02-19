import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import * as Zod from "@moq/lite/zod";
import { Effect, Signal } from "@moq/signals";

export interface PeersProps {
	enabled?: boolean | Signal<boolean>;
	positions?: Record<string, Catalog.Position> | Signal<Record<string, Catalog.Position>>;
}

export class Peers {
	static readonly TRACK = "location/peers.json";
	static readonly PRIORITY = Catalog.PRIORITY.location;

	enabled: Signal<boolean>;
	positions = new Signal<Record<string, Catalog.Position>>({});

	catalog = new Signal<Catalog.Track | undefined>(undefined);
	signals = new Effect();

	constructor(props?: PeersProps) {
		this.enabled = Signal.from(props?.enabled ?? false);
		this.positions = Signal.from(props?.positions ?? {});

		this.signals.run((effect) => {
			const enabled = effect.get(this.enabled);
			if (!enabled) return;

			effect.set(this.catalog, { name: Peers.TRACK });
		});
	}

	serve(track: Moq.Track, effect: Effect): void {
		const values = effect.getAll([this.enabled, this.positions]);
		if (!values) return;
		const [_, positions] = values;

		Zod.write(track, positions, Catalog.PeersSchema);
	}

	close() {
		this.signals.close();
	}
}
