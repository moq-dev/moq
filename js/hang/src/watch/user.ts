import { Effect, type Getter, Signal } from "@moq/signals";
import type * as Catalog from "../catalog";

export interface Props {
	enabled?: boolean | Signal<boolean>;
}

export class Info {
	enabled: Signal<boolean>;

	#id = new Signal<string | undefined>(undefined);
	#name = new Signal<string | undefined>(undefined);
	#avatar = new Signal<string | undefined>(undefined);
	#color = new Signal<string | undefined>(undefined);

	signals = new Effect();

	constructor(catalog: Signal<Catalog.Root | undefined>, props?: Props) {
		this.enabled = Signal.from(props?.enabled ?? false);

		this.signals.effect((effect) => {
			if (!effect.get(this.enabled)) return;

			this.#id.set(effect.get(catalog)?.user?.id);
			this.#name.set(effect.get(catalog)?.user?.name);
			this.#avatar.set(effect.get(catalog)?.user?.avatar);
			this.#color.set(effect.get(catalog)?.user?.color);
		});
	}

	get id(): Getter<string | undefined> {
		return this.#id;
	}

	get name(): Getter<string | undefined> {
		return this.#name;
	}

	get avatar(): Getter<string | undefined> {
		return this.#avatar;
	}

	get color(): Getter<string | undefined> {
		return this.#color;
	}

	close() {
		this.signals.close();
	}
}
