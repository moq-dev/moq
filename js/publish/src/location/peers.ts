import { PRIORITY, type Track } from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import type { Position } from "./types";

export interface PeersProps {
	enabled?: boolean | Signal<boolean>;
	positions?: Record<string, Position> | Signal<Record<string, Position>>;
}

export class Peers {
	static readonly TRACK = "location/peers.json";
	static readonly PRIORITY = PRIORITY.location;

	enabled: Signal<boolean>;
	positions = new Signal<Record<string, Position>>({});

	catalog = new Signal<Track | undefined>(undefined);
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

		track.writeJson(positions);
	}

	close() {
		this.signals.close();
	}
}
