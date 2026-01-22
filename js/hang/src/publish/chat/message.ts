import type * as Moq from "@moq/lite";
import { Effect, Signal } from "@moq/signals";
import * as Catalog from "../../catalog";

export type MessageProps = {
	enabled?: boolean | Signal<boolean>;
};

export class Message {
	static readonly TRACK = "chat/message.txt";
	static readonly PRIORITY = Catalog.PRIORITY.chat;

	enabled: Signal<boolean>;

	// The latest message to publish.
	latest: Signal<string>;

	catalog = new Signal<Catalog.Track | undefined>(undefined);

	#signals = new Effect();

	constructor(props?: MessageProps) {
		this.enabled = Signal.from(props?.enabled ?? false);
		this.latest = new Signal<string>("");

		this.#signals.effect((effect) => {
			const enabled = effect.get(this.enabled);
			if (!enabled) return;

			effect.set(this.catalog, { name: Message.TRACK, priority: Message.PRIORITY });
		});
	}

	serve(track: Moq.Track, effect: Effect): void {
		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const latest = effect.get(this.latest);
		track.writeString(latest ?? "");
	}

	close() {
		this.#signals.close();
	}
}
