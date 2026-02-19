import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";

export type TypingProps = {
	enabled?: boolean | Signal<boolean>;
};

export class Typing {
	static readonly TRACK = "chat/typing.bool";
	static readonly PRIORITY = Catalog.PRIORITY.typing;

	enabled: Signal<boolean>;

	// Whether the user is typing.
	active: Signal<boolean>;

	catalog = new Signal<Catalog.Track | undefined>(undefined);

	#signals = new Effect();

	constructor(props?: TypingProps) {
		this.enabled = Signal.from(props?.enabled ?? false);
		this.active = new Signal<boolean>(false);

		this.#signals.run((effect) => {
			const enabled = effect.get(this.enabled);
			if (!enabled) return;

			effect.set(this.catalog, { name: Typing.TRACK });
		});
	}

	serve(track: Moq.Track, effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const active = effect.get(this.active);
		track.writeBool(active);
	}

	close() {
		this.#signals.close();
	}
}
