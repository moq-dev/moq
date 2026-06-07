import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import * as Zod from "@moq/net/zod";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";

export type PeersInput = {
	broadcast: Getter<Moq.Broadcast | undefined>;
	catalog: Getter<Catalog.Root | undefined>;
	enabled: Getter<boolean>;
};

type PeersOutput = {
	positions: Signal<Record<string, Catalog.Position> | undefined>;
};

export class Peers {
	readonly input: Readonlys<PeersInput>;

	readonly #output: PeersOutput = {
		positions: new Signal<Record<string, Catalog.Position> | undefined>(undefined),
	};
	readonly output = readonlys(this.#output);

	#catalog = new Signal<Catalog.Track | undefined>(undefined);

	signals = new Effect();

	constructor(props?: Inputs<PeersInput>) {
		this.input = {
			broadcast: getter(props?.broadcast),
			catalog: getter(props?.catalog),
			enabled: getter(props?.enabled ?? false),
		};

		this.signals.run((effect) => {
			this.#catalog.set(effect.get(this.input.catalog)?.location?.peers);
		});

		this.signals.run(this.#run.bind(this));
	}

	#run(effect: Effect) {
		const values = effect.getAll([this.input.enabled, this.#catalog, this.input.broadcast]);
		if (!values) return;
		const [_, catalog, broadcast] = values;

		const track = broadcast.track(catalog.name).subscribe({ priority: Catalog.PRIORITY.location });
		effect.cleanup(() => track.close());

		effect.spawn(this.#runTrack.bind(this, track));
	}

	async #runTrack(track: Moq.TrackSubscriber) {
		try {
			for (;;) {
				const frame = await Zod.read(track, Catalog.PeersSchema);
				if (!frame) break;

				this.#output.positions.set(frame);
			}
		} finally {
			this.#output.positions.set(undefined);
			track.close();
		}
	}

	close() {
		this.signals.close();
	}
}
