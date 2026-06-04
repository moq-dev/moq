import type * as Catalog from "@moq/hang/catalog";
import type * as Moq from "@moq/net";
import { Effect, type Getter, getter, type Inputs, type Readonlys } from "@moq/signals";
import { Peers, type PeersInput } from "./peers";
import { Window, type WindowInput } from "./window";

type RootInput = {
	broadcast: Getter<Moq.Broadcast | undefined>;
	catalog: Getter<Catalog.Root | undefined>;
};

export class Root {
	readonly input: Readonlys<RootInput>;

	window: Window;
	peers: Peers;

	signals = new Effect();

	constructor(props?: Inputs<RootInput> & { window?: Inputs<WindowInput>; peers?: Inputs<PeersInput> }) {
		this.input = {
			broadcast: getter(props?.broadcast),
			catalog: getter(props?.catalog),
		};

		this.window = new Window({ ...props?.window, broadcast: this.input.broadcast, catalog: this.input.catalog });
		this.peers = new Peers({ ...props?.peers, broadcast: this.input.broadcast, catalog: this.input.catalog });
	}

	close() {
		this.signals.close();
		this.window.close();
		this.peers.close();
	}
}
