import { Effect, Signal } from "@moq/signals";
import type * as Catalog from "../catalog";

export type Props = {
	enabled?: boolean | Signal<boolean>;
	id?: string | Signal<string | undefined>;
	name?: string | Signal<string | undefined>;
	avatar?: string | Signal<string | undefined>;
	color?: string | Signal<string | undefined>;
};

export class Info {
	enabled: Signal<boolean>;

	id: Signal<string | undefined>;
	name: Signal<string | undefined>;
	avatar: Signal<string | undefined>;
	color: Signal<string | undefined>;

	catalog = new Signal<Catalog.User | undefined>(undefined);

	signals = new Effect();

	constructor(props?: Props) {
		this.enabled = Signal.from(props?.enabled ?? false);
		this.id = Signal.from(props?.id);
		this.name = Signal.from(props?.name);
		this.avatar = Signal.from(props?.avatar);
		this.color = Signal.from(props?.color);

		this.signals.effect((effect) => {
			if (!effect.get(this.enabled)) return;

			effect.set(this.catalog, {
				id: effect.get(this.id),
				name: effect.get(this.name),
				avatar: effect.get(this.avatar),
				color: effect.get(this.color),
			});
		});
	}

	close() {
		this.signals.close();
	}
}
