import * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Effect, type Getter, getter, type InputProps, type Readonlys, readonlys, Signal } from "@moq/signals";

// Signals the component reads. Whoever owns the backing Signal does the writing.
type TypingInput = {
	broadcast: Getter<Moq.Broadcast | undefined>;

	// The catalog to grab the chat section from.
	catalog: Getter<Catalog.Root | undefined>;

	// Whether to start downloading the chat.
	// Defaults to false so you can make sure everything is ready before starting.
	enabled: Getter<boolean>;
};

type TypingOutput = {
	active: Signal<boolean | undefined>;

	catalog: Signal<Catalog.Track | undefined>;
};

export type TypingProps = InputProps<TypingInput>;

export class Typing {
	readonly input: Readonlys<TypingInput>;

	readonly #output: TypingOutput = {
		active: new Signal<boolean | undefined>(undefined),
		catalog: new Signal<Catalog.Track | undefined>(undefined),
	};
	readonly output = readonlys(this.#output);

	#signals = new Effect();

	constructor(props?: TypingProps) {
		this.input = {
			broadcast: getter(props?.broadcast),
			catalog: getter(props?.catalog),
			enabled: getter(props?.enabled ?? false),
		};

		// Grab the chat section from the catalog (if it's changed).
		this.#signals.run((effect) => {
			if (!effect.get(this.input.enabled)) return;
			this.#output.catalog.set(effect.get(this.input.catalog)?.chat?.typing);
		});

		this.#signals.run(this.#run.bind(this));
	}

	#run(effect: Effect) {
		const values = effect.getAll([this.input.enabled, this.#output.catalog, this.input.broadcast]);
		if (!values) return;
		const [_, catalog, broadcast] = values;

		const track = broadcast.subscribe(catalog.name, Catalog.PRIORITY.typing);
		effect.cleanup(() => track.close());

		effect.spawn(async () => {
			for (;;) {
				const value = await track.readBool();
				if (value === undefined) break;

				this.#output.active.set(value);
			}
		});

		effect.cleanup(() => this.#output.active.set(undefined));
	}

	close() {
		this.#signals.close();
	}
}
