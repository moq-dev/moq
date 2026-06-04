import type * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys, readonlys, Signal } from "@moq/signals";
import { Message, type MessageInput } from "./message";
import { Typing, type TypingInput } from "./typing";

// Signals the component reads. Whoever owns the backing Signal does the writing.
type ChatInput = {
	broadcast: Getter<Moq.Broadcast | undefined>;
	catalog: Getter<Catalog.Root | undefined>;
};

type ChatOutput = {
	catalog: Signal<Catalog.Chat | undefined>;
};

export class Chat {
	readonly input: Readonlys<ChatInput>;

	readonly #output: ChatOutput = {
		catalog: new Signal<Catalog.Chat | undefined>(undefined),
	};
	readonly output = readonlys(this.#output);

	message: Message;
	typing: Typing;

	#signals = new Effect();

	constructor(props?: Inputs<ChatInput> & { message?: Inputs<MessageInput>; typing?: Inputs<TypingInput> }) {
		this.input = {
			broadcast: getter(props?.broadcast),
			catalog: getter(props?.catalog),
		};

		this.message = new Message({
			...props?.message,
			broadcast: this.input.broadcast,
			catalog: this.input.catalog,
		});
		this.typing = new Typing({
			...props?.typing,
			broadcast: this.input.broadcast,
			catalog: this.input.catalog,
		});

		// Grab the chat section from the catalog (if it's changed).
		this.#signals.run((effect) => {
			const message = effect.get(this.message.output.catalog);
			const typing = effect.get(this.typing.output.catalog);
			if (!message && !typing) return;

			effect.set(this.#output.catalog, {
				message,
				typing,
			});
		});
	}

	close() {
		this.#signals.close();
		this.message.close();
		this.typing.close();
	}
}
