import type * as Moq from "@moq/lite";
import { Effect, type Getter, type Signal } from "@moq/signals";
import type { Location as LocationSection } from "../sections";
import { Peers, type PeersProps } from "./peers";
import { Window, type WindowProps } from "./window";

export interface Props {
	window?: WindowProps;
	peers?: PeersProps;
}

export class Root {
	window: Window;
	peers: Peers;

	signals = new Effect();

	constructor(
		broadcast: Signal<Moq.Broadcast | undefined>,
		locationSection: Getter<LocationSection | undefined>,
		props?: Props,
	) {
		this.window = new Window(broadcast, locationSection, props?.window);
		this.peers = new Peers(broadcast, locationSection, props?.peers);
	}

	close() {
		this.signals.close();
		this.window.close();
		this.peers.close();
	}
}
