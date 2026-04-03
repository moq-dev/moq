import type { Track } from "@moq/hang/catalog";
import { Effect, type Getter, Signal } from "@moq/signals";
import { Message, type MessageProps } from "./message";
import { Typing, type TypingProps } from "./typing";

export * from "./message";
export * from "./typing";

export type Props = {
	message?: MessageProps;
	typing?: TypingProps;
};

export type ChatCatalog = {
	message?: Track;
	typing?: Track;
};

export class Root {
	message: Message;
	typing: Typing;

	#catalog = new Signal<ChatCatalog | undefined>(undefined);
	readonly catalog: Getter<ChatCatalog | undefined> = this.#catalog;

	#signals = new Effect();

	constructor(props?: Props) {
		this.message = new Message(props?.message);
		this.typing = new Typing(props?.typing);

		this.#signals.run((effect) => {
			this.#catalog.set({
				message: effect.get(this.message.catalog),
				typing: effect.get(this.typing.catalog),
			});
		});
	}

	close() {
		this.#signals.close();
		this.message.close();
		this.typing.close();
	}
}
