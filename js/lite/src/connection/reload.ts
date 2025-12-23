import { Effect, Signal } from "@moq/signals";
import type { Time } from "../index.ts";
import { type ConnectProps, connect, type WebSocketOptions } from "./connect.ts";
import type { Established } from "./established.ts";

export type ReloadDelay = {
	// The delay in milliseconds before reconnecting.
	// default: 1000
	initial: Time.Milli;

	// The multiplier for the delay.
	// default: 2
	multiplier: number;

	// The maximum delay in milliseconds.
	// default: 30000
	max: Time.Milli;
};

export type ReloadProps = ConnectProps & {
	// Whether to reload the connection when it disconnects.
	// default: true
	enabled?: boolean | Signal<boolean>;

	// The URL of the relay server.
	url?: URL | Signal<URL | undefined>;

	// The delay for the reload.
	delay?: ReloadDelay;
};

export type ReloadStatus = "connecting" | "connected" | "disconnected";

export class Reload {
	url: Signal<URL | undefined>;
	enabled: Signal<boolean>;

	status = new Signal<ReloadStatus>("disconnected");
	established = new Signal<Established | undefined>(undefined);

	// WebTransport options (not reactive).
	webtransport?: WebTransportOptions;

	// WebSocket (fallback) options (not reactive).
	websocket: WebSocketOptions | undefined;

	// Not reactive, but can be updated.
	delay: ReloadDelay;

	signals = new Effect();

	#delay: Time.Milli;

	// Increased by 1 each time to trigger a reload.
	#tick = new Signal(0);

	constructor(props?: ReloadProps) {
		this.url = Signal.from(props?.url);
		this.enabled = Signal.from(props?.enabled ?? false);
		this.delay = props?.delay ?? { initial: 1000 as Time.Milli, multiplier: 2, max: 30000 as Time.Milli };
		this.webtransport = props?.webtransport;
		this.websocket = props?.websocket;

		this.#delay = this.delay.initial;

		// Create a reactive root so cleanup is easier.
		this.signals.effect(this.#connect.bind(this));
	}

	#connect(effect: Effect): void {
		// Will retry when the tick changes.
		effect.get(this.#tick);

		const enabled = effect.get(this.enabled);
		if (!enabled) return;

		const url = effect.get(this.url);
		if (!url) return;

		effect.set(this.status, "connecting", "disconnected");

		effect.spawn(async () => {
			try {
				const pending = connect(url, { websocket: this.websocket, webtransport: this.webtransport });

				const connection = await Promise.race([effect.cancel, pending]);
				if (!connection) {
					pending.then((conn) => conn.close()).catch(() => {});
					return;
				}

				effect.set(this.established, connection);
				effect.cleanup(() => connection.close());

				effect.set(this.status, "connected", "disconnected");

				// Reset the exponential backoff on success.
				this.#delay = this.delay.initial;

				await Promise.race([effect.cancel, connection.closed]);
			} catch (err) {
				console.warn("connection error:", err);

				const tick = this.#tick.peek() + 1;
				effect.timer(() => this.#tick.update((prev) => Math.max(prev, tick)), this.#delay);

				this.#delay = Math.min(this.#delay * this.delay.multiplier, this.delay.max) as Time.Milli;
			}
		});
	}

	close() {
		this.signals.close();
	}
}
