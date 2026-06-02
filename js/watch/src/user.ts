import type * as Catalog from "@moq/hang/catalog";
import { Effect, type Getter, getter, type InputProps, type Readonlys, readonlys, Signal } from "@moq/signals";

type InfoInput = {
	enabled: Getter<boolean>;
	catalog: Getter<Catalog.Root | undefined>;
};

type InfoOutput = {
	id: Signal<string | undefined>;
	name: Signal<string | undefined>;
	avatar: Signal<string | undefined>;
	color: Signal<string | undefined>;
};

export type Props = InputProps<InfoInput>;

export class Info {
	readonly input: Readonlys<InfoInput>;

	readonly #output: InfoOutput = {
		id: new Signal<string | undefined>(undefined),
		name: new Signal<string | undefined>(undefined),
		avatar: new Signal<string | undefined>(undefined),
		color: new Signal<string | undefined>(undefined),
	};
	readonly output = readonlys(this.#output);

	signals = new Effect();

	constructor(props?: Props) {
		this.input = {
			enabled: getter(props?.enabled ?? false),
			catalog: getter(props?.catalog),
		};

		this.signals.run((effect) => {
			if (!effect.get(this.input.enabled)) return;

			this.#output.id.set(effect.get(this.input.catalog)?.user?.id);
			this.#output.name.set(effect.get(this.input.catalog)?.user?.name);
			this.#output.avatar.set(effect.get(this.input.catalog)?.user?.avatar);
			this.#output.color.set(effect.get(this.input.catalog)?.user?.color);
		});
	}

	close() {
		this.signals.close();
	}
}
