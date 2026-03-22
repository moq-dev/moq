import { PRIORITY, type Track } from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Chat } from "../sections";

export interface TypingProps {
	// Whether to start downloading the chat.
	// Defaults to false so you can make sure everything is ready before starting.
	enabled?: boolean | Signal<boolean>;
}

export class Typing {
	broadcast: Signal<Moq.Broadcast | undefined>;
	enabled: Signal<boolean>;
	active: Signal<boolean | undefined>;

	#catalog = new Signal<Track | undefined>(undefined);
	readonly catalog: Getter<Track | undefined> = this.#catalog;

	#signals = new Effect();

	constructor(
		broadcast: Signal<Moq.Broadcast | undefined>,
		chatSection: Getter<Chat | undefined>,
		props?: TypingProps,
	) {
		this.broadcast = broadcast;
		this.active = new Signal<boolean | undefined>(undefined);
		this.enabled = Signal.from(props?.enabled ?? false);

		// Grab the chat.typing track from the catalog section (if it's changed).
		this.#signals.run((effect) => {
			if (!effect.get(this.enabled)) return;
			this.#catalog.set(effect.get(chatSection)?.typing);
		});

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect) {
		const values = effect.getAll([this.enabled, this.#catalog, this.broadcast]);
		if (!values) return;
		const [_, catalog, broadcast] = values;

		const track = broadcast.subscribe(catalog.name, PRIORITY.typing);
		effect.cleanup(() => track.close());

		effect.spawn(async () => {
			for (;;) {
				const value = await track.readBool();
				if (value === undefined) break;

				this.active.set(value);
			}
		});

		effect.cleanup(() => this.active.set(undefined));
	}

	close() {
		this.#signals.close();
	}
}
