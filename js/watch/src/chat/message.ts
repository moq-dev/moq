import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Effect, type Getter, getter, type InputProps, type Readonlys, readonlys, Signal } from "@moq/signals";

// Signals the component reads. Whoever owns the backing Signal does the writing.
type MessageInput = {
	broadcast: Getter<Moq.Broadcast | undefined>;

	// The catalog to grab the chat section from.
	catalog: Getter<Catalog.Root | undefined>;

	// Whether to start downloading the chat.
	// Defaults to false so you can make sure everything is ready before starting.
	enabled: Getter<boolean>;
};

type MessageOutput = {
	// Empty string is a valid message.
	latest: Signal<string | undefined>;

	catalog: Signal<Catalog.Track | undefined>;
};

export type MessageProps = InputProps<MessageInput>;

export class Message {
	readonly input: Readonlys<MessageInput>;

	readonly #output: MessageOutput = {
		latest: new Signal<string | undefined>(undefined),
		catalog: new Signal<Catalog.Track | undefined>(undefined),
	};
	readonly output = readonlys(this.#output);

	#signals = new Effect();

	constructor(props?: MessageProps) {
		this.input = {
			broadcast: getter(props?.broadcast),
			catalog: getter(props?.catalog),
			enabled: getter(props?.enabled ?? false),
		};

		// Grab the chat section from the catalog (if it's changed).
		this.#signals.run((effect) => {
			if (!effect.get(this.input.enabled)) return;
			this.#output.catalog.set(effect.get(this.input.catalog)?.chat?.message);
		});

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect) {
		const values = effect.getAll([this.input.enabled, this.#output.catalog, this.input.broadcast]);
		if (!values) return;
		const [_, catalog, broadcast] = values;

		const track = broadcast.subscribe(catalog.name, Catalog.PRIORITY.chat);
		effect.cleanup(() => track.close());

		// Undefined is only when we're not subscribed to the track.
		effect.set(this.#output.latest, "");
		effect.cleanup(() => this.#output.latest.set(undefined));

		effect.spawn(async () => {
			for (;;) {
				const frame = await track.readString();
				if (frame === undefined) break;

				// Use a function to avoid the dequal check.
				this.#output.latest.set(frame);
			}
		});
	}

	close() {
		this.#signals.close();
	}
}
