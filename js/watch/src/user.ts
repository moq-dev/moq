import { Effect, type Getter, Signal } from "@moq/signals";
import type { User } from "./sections";

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

	constructor(user: Getter<User | undefined>, props?: Props) {
		this.enabled = Signal.from(props?.enabled ?? false);

		this.signals.run((effect) => {
			if (!effect.get(this.enabled)) return;

			const u = effect.get(user);
			this.#id.set(u?.id);
			this.#name.set(u?.name);
			this.#avatar.set(u?.avatar);
			this.#color.set(u?.color);
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
