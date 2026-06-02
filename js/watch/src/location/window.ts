import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import * as Zod from "@moq/net/zod";
import { Effect, type Getter, getter, type InputProps, type Readonlys, readonlys, Signal } from "@moq/signals";

type WindowInput = {
	broadcast: Getter<Moq.Broadcast | undefined>;
	catalog: Getter<Catalog.Root | undefined>;
	enabled: Getter<boolean>;
};

type WindowOutput = {
	handle: Signal<string | undefined>;
	position: Signal<Catalog.Position | undefined>;
};

export type WindowProps = InputProps<WindowInput>;

export class Window {
	readonly input: Readonlys<WindowInput>;

	readonly #output: WindowOutput = {
		handle: new Signal<string | undefined>(undefined),
		position: new Signal<Catalog.Position | undefined>(undefined),
	};
	readonly output = readonlys(this.#output);

	#catalog = new Signal<Catalog.Location | undefined>(undefined);

	signals = new Effect();

	constructor(props?: WindowProps) {
		this.input = {
			broadcast: getter(props?.broadcast),
			catalog: getter(props?.catalog),
			enabled: getter(props?.enabled ?? false),
		};

		this.signals.run((effect) => {
			this.#catalog.set(effect.get(this.input.catalog)?.location);
		});

		this.signals.run((effect) => {
			if (!effect.get(this.input.enabled)) return;
			this.#output.position.set(effect.get(this.#catalog)?.initial);
		});

		this.signals.run((effect) => {
			this.#output.handle.set(effect.get(this.#catalog)?.handle);
		});

		this.signals.run((effect) => {
			const broadcast = effect.get(this.input.broadcast);
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

				this.#output.position.set(position);
			}
		} finally {
			this.#output.position.set(undefined);
			track.close();
		}
	}

	close() {
		this.signals.close();
	}
}
