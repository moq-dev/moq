import { Effect, Signal } from "@moq/signals";
import type { Catalog } from "../..";
import { Peers, type PeersProps } from "./peers";
import { Window, type WindowProps } from "./window";

export * from "./peers";
export * from "./window";

export type Props = {
	window?: WindowProps;
	peers?: PeersProps;
};

export class Root {
	window: Window;
	peers: Peers;

	catalog = new Signal<Catalog.Location | undefined>(undefined);
	signals = new Effect();

	constructor(props?: Props) {
		this.window = new Window(props?.window);
		this.peers = new Peers(props?.peers);

		this.signals.effect(this.#run.bind(this));
	}

	#run(effect: Effect): void {
		const myself = effect.get(this.window.catalog);
		const peers = effect.get(this.peers.catalog);
		if (!myself && !peers) return;

		effect.set(this.catalog, {
			peers: peers,
			...myself,
		});
	}

	close() {
		this.signals.close();
		this.window.close();
		this.peers.close();
	}
}
