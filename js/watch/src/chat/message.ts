import { PRIORITY, type Track } from "@moq/hang/catalog";
import type * as Moq from "@moq/lite";
import { Effect, type Getter, Signal } from "@moq/signals";
import type { Chat } from "../sections";

export interface MessageProps {
	// Whether to start downloading the chat.
	// Defaults to false so you can make sure everything is ready before starting.
	enabled?: boolean | Signal<boolean>;
}

export class Message {
	broadcast: Signal<Moq.Broadcast | undefined>;
	enabled: Signal<boolean>;

	// Empty string is a valid message.
	#latest = new Signal<string | undefined>(undefined);
	readonly latest: Getter<string | undefined> = this.#latest;

	#catalog = new Signal<Track | undefined>(undefined);
	readonly catalog: Getter<Track | undefined> = this.#catalog;

	#signals = new Effect();

	constructor(
		broadcast: Signal<Moq.Broadcast | undefined>,
		chatSection: Getter<Chat | undefined>,
		props?: MessageProps,
	) {
		this.broadcast = broadcast;
		this.enabled = Signal.from(props?.enabled ?? false);

		// Grab the chat.message track from the catalog section (if it's changed).
		this.#signals.run((effect) => {
			if (!effect.get(this.enabled)) return;
			this.#catalog.set(effect.get(chatSection)?.message);
		});

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect) {
		const values = effect.getAll([this.enabled, this.#catalog, this.broadcast]);
		if (!values) return;
		const [_, catalog, broadcast] = values;

		const track = broadcast.subscribe(catalog.name, PRIORITY.chat);
		effect.cleanup(() => track.close());

		// Undefined is only when we're not subscribed to the track.
		effect.set(this.#latest, "");
		effect.cleanup(() => this.#latest.set(undefined));

		effect.spawn(async () => {
			for (;;) {
				const frame = await track.readString();
				if (frame === undefined) break;

				// Use a function to avoid the dequal check.
				this.#latest.set(frame);
			}
		});
	}

	close() {
		this.#signals.close();
	}
}
